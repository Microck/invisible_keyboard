//! Linux controller-source opening and serial-line discipline.

#![cfg(target_os = "linux")]

use std::{
    error::Error,
    fmt,
    fs::{File, OpenOptions},
    io::{self, IsTerminal, Read, Write},
    mem::MaybeUninit,
    os::{
        fd::{AsRawFd, FromRawFd, RawFd},
        unix::{fs::FileTypeExt, fs::OpenOptionsExt},
    },
    path::{Path, PathBuf},
};

/// USB CDC line speed used for each controller stream.
///
/// One hand produces at most 32,400 transport bytes per second. A conventional
/// 8N1 line needs ten bits per byte, so 921,600 baud leaves substantial margin
/// above the 324,000-bit/s wire requirement.
pub const CONTROLLER_TTY_BAUD_RATE: u32 = 921_600;

/// Physical object backing an opened controller source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControllerSourceKind {
    /// A seekable replay or test fixture.
    RegularFile,
    /// A live byte-stream fixture with ordinary FIFO blocking semantics.
    Fifo,
    /// A character device proven to be a terminal and configured below.
    Tty,
}

/// Parity discipline applied to a controller TTY.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControllerTtyParity {
    None,
}

/// Effective raw discipline required from a controller TTY.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RawControllerTtyDiscipline {
    pub baud_rate: u32,
    pub data_bits: u8,
    pub parity: ControllerTtyParity,
    pub stop_bits: u8,
    pub receiver_enabled: bool,
    pub local_mode: bool,
    pub software_flow_control: bool,
    pub hardware_flow_control: bool,
    pub minimum_read_bytes: u8,
    pub read_timeout_deciseconds: u8,
}

/// Canonical controller TTY discipline.
pub const RAW_CONTROLLER_TTY_DISCIPLINE: RawControllerTtyDiscipline = RawControllerTtyDiscipline {
    baud_rate: CONTROLLER_TTY_BAUD_RATE,
    data_bits: 8,
    parity: ControllerTtyParity::None,
    stop_bits: 1,
    receiver_enabled: true,
    local_mode: true,
    software_flow_control: false,
    hardware_flow_control: false,
    minimum_read_bytes: 1,
    read_timeout_deciseconds: 0,
};

/// Discipline applied while opening one source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControllerSourceDiscipline {
    /// No terminal operations were attempted. Bytes pass through unchanged.
    PassThrough,
    /// The terminal was set and read back as the canonical raw discipline.
    RawTty(RawControllerTtyDiscipline),
}

/// Auditable result of opening and classifying one source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ControllerSourceReport {
    pub path: PathBuf,
    pub kind: ControllerSourceKind,
    pub discipline: ControllerSourceDiscipline,
}

/// An open byte source plus the discipline proven at open time.
#[derive(Debug)]
pub struct OpenedControllerSource {
    pub file: File,
    pub report: ControllerSourceReport,
}

/// One exclusively owned, duplex Gate 0A controller connection.
///
/// Both descriptors refer to the same open TTY description. The reader can
/// block in its acquisition thread while the writer sends the one terminal
/// request and drains it without opening the device a second time.
#[derive(Debug)]
pub struct OpenedGate0Controller {
    pub reader: File,
    pub writer: Gate0ControllerWriter,
    pub report: ControllerSourceReport,
}

/// One nonblocking observation from the private Gate 0A stop/guard pipe.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Gate0StopPipeRead {
    WouldBlock,
    Bytes(Vec<u8>),
    Eof,
}

/// The two inherited pipe ends owned by the Gate 0A recorder process.
#[derive(Debug)]
pub struct Gate0ControlPipes {
    ready: Option<File>,
    stop: File,
}

impl Gate0ControlPipes {
    /// Adopt and validate the wrapper's inherited pipe descriptors.
    ///
    /// The ready descriptor must be a write-only pipe and the stop descriptor
    /// must be a read-only pipe. Standard descriptors and aliases are rejected
    /// before ownership transfers. The stop side becomes nonblocking so device
    /// bytes, signals, deadlines, and analyzer state share one bounded loop.
    ///
    /// # Safety
    ///
    /// The caller must own both live descriptors and must not use or close them
    /// after this call, including when it returns an error after adoption.
    #[allow(unsafe_code)]
    pub unsafe fn from_inherited_descriptors(
        ready_descriptor: RawFd,
        stop_descriptor: RawFd,
    ) -> Result<Self, Gate0ControlPipeError> {
        if ready_descriptor <= libc::STDERR_FILENO || stop_descriptor <= libc::STDERR_FILENO {
            return Err(Gate0ControlPipeError::StandardDescriptor);
        }
        if ready_descriptor == stop_descriptor {
            return Err(Gate0ControlPipeError::AliasedDescriptors);
        }

        // SAFETY: the function contract transfers ownership of both live,
        // distinct descriptors. Each is converted exactly once.
        let ready = unsafe { File::from_raw_fd(ready_descriptor) };
        // SAFETY: as above, the stop descriptor is independently owned.
        let stop = unsafe { File::from_raw_fd(stop_descriptor) };
        validate_pipe_descriptor(&ready, Gate0ControlPipeEnd::ReadyWrite)?;
        validate_pipe_descriptor(&stop, Gate0ControlPipeEnd::StopRead)?;
        set_close_on_exec(&ready, Gate0ControlPipeEnd::ReadyWrite)?;
        set_close_on_exec(&stop, Gate0ControlPipeEnd::StopRead)?;
        set_nonblocking(&stop, Gate0ControlPipeEnd::StopRead)?;
        Ok(Self {
            ready: Some(ready),
            stop,
        })
    }

    /// Publish the sole readiness byte and close the ready descriptor.
    pub fn publish_ready(&mut self) -> Result<(), Gate0ControlPipeError> {
        let mut ready = self
            .ready
            .take()
            .ok_or(Gate0ControlPipeError::ReadinessAlreadyPublished)?;
        ready
            .write_all(&[1])
            .map_err(|source| Gate0ControlPipeError::Io {
                end: Gate0ControlPipeEnd::ReadyWrite,
                operation: "write readiness byte",
                source,
            })?;
        Ok(())
    }

    /// Read all stop bytes currently available, one bounded chunk at a time.
    pub fn read_stop(&mut self) -> Result<Gate0StopPipeRead, Gate0ControlPipeError> {
        let mut bytes = [0_u8; 64];
        match self.stop.read(&mut bytes) {
            Ok(0) => Ok(Gate0StopPipeRead::Eof),
            Ok(length) => Ok(Gate0StopPipeRead::Bytes(bytes[..length].to_vec())),
            Err(source) if source.kind() == io::ErrorKind::WouldBlock => {
                Ok(Gate0StopPipeRead::WouldBlock)
            }
            Err(source) if source.kind() == io::ErrorKind::Interrupted => {
                Ok(Gate0StopPipeRead::WouldBlock)
            }
            Err(source) => Err(Gate0ControlPipeError::Io {
                end: Gate0ControlPipeEnd::StopRead,
                operation: "read stop/guard pipe",
                source,
            }),
        }
    }
}

/// Private pipe end named in a control-pipe diagnostic.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Gate0ControlPipeEnd {
    ReadyWrite,
    StopRead,
}

impl fmt::Display for Gate0ControlPipeEnd {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ReadyWrite => "ready write",
            Self::StopRead => "stop read",
        })
    }
}

/// Invalid ownership or I/O on the private Gate 0A control pipes.
#[derive(Debug)]
pub enum Gate0ControlPipeError {
    StandardDescriptor,
    AliasedDescriptors,
    NotPipe(Gate0ControlPipeEnd),
    WrongAccessMode(Gate0ControlPipeEnd),
    ReadinessAlreadyPublished,
    Io {
        end: Gate0ControlPipeEnd,
        operation: &'static str,
        source: io::Error,
    },
}

impl fmt::Display for Gate0ControlPipeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StandardDescriptor => {
                formatter.write_str("Gate 0A control pipes must not reuse stdin, stdout, or stderr")
            }
            Self::AliasedDescriptors => {
                formatter.write_str("Gate 0A ready and stop descriptors must differ")
            }
            Self::NotPipe(end) => write!(formatter, "Gate 0A {end} descriptor is not a pipe"),
            Self::WrongAccessMode(end) => {
                write!(
                    formatter,
                    "Gate 0A {end} descriptor has the wrong access mode"
                )
            }
            Self::ReadinessAlreadyPublished => {
                formatter.write_str("Gate 0A readiness was already published")
            }
            Self::Io {
                end,
                operation,
                source,
            } => write!(
                formatter,
                "failed to {operation} on Gate 0A {end} descriptor: {source}"
            ),
        }
    }
}

impl Error for Gate0ControlPipeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::StandardDescriptor
            | Self::AliasedDescriptors
            | Self::NotPipe(_)
            | Self::WrongAccessMode(_)
            | Self::ReadinessAlreadyPublished => None,
        }
    }
}

/// Independently owned host-to-controller half of a Gate 0A TTY.
#[derive(Debug)]
pub struct Gate0ControllerWriter {
    file: File,
    path: PathBuf,
}

impl Gate0ControllerWriter {
    /// Wait for all request bytes queued in the host TTY driver.
    pub fn drain_output(&self) -> Result<(), ControllerSourceError> {
        drain_output(&self.path, self.file.as_raw_fd())
    }
}

impl Write for Gate0ControllerWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.file.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

impl OpenedControllerSource {
    /// Consume the report wrapper and return the configured stream.
    #[must_use]
    pub fn into_file(self) -> File {
        self.file
    }
}

/// System operation that failed while opening or configuring a source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControllerSourceOperation {
    Open,
    InspectFileType,
    ReadTermios,
    SetInputSpeed,
    SetOutputSpeed,
    ApplyTermios,
    ReadBackTermios,
    AcquireExclusiveAccess,
    DuplicateDuplexDescriptor,
    DrainOutput,
}

impl fmt::Display for ControllerSourceOperation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Open => "open",
            Self::InspectFileType => "inspect file type",
            Self::ReadTermios => "read terminal settings",
            Self::SetInputSpeed => "set terminal input speed",
            Self::SetOutputSpeed => "set terminal output speed",
            Self::ApplyTermios => "apply terminal settings",
            Self::ReadBackTermios => "read back terminal settings",
            Self::AcquireExclusiveAccess => "acquire exclusive terminal access",
            Self::DuplicateDuplexDescriptor => "duplicate the duplex terminal descriptor",
            Self::DrainOutput => "drain terminal output",
        })
    }
}

/// A source was unsafe or could not be prepared for byte-exact acquisition.
#[derive(Debug)]
pub enum ControllerSourceError {
    Io {
        path: PathBuf,
        operation: ControllerSourceOperation,
        source: io::Error,
    },
    /// Character devices are accepted only when termios can govern their bytes.
    CharacterDeviceIsNotTty { path: PathBuf },
    /// Only regular files, FIFOs, and proven TTYs are valid byte sources.
    UnsupportedFileType { path: PathBuf },
    /// Gate 0A is a physical duplex protocol and cannot use replay fixtures.
    Gate0RequiresTty { path: PathBuf },
    /// The driver did not retain every required setting after `tcsetattr`.
    RawDisciplineReadbackMismatch { path: PathBuf },
}

impl fmt::Display for ControllerSourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io {
                path,
                operation,
                source,
            } => write!(
                formatter,
                "failed to {operation} for controller source {}: {source}",
                path.display()
            ),
            Self::CharacterDeviceIsNotTty { path } => write!(
                formatter,
                "controller source {} is a character device but not a TTY",
                path.display()
            ),
            Self::UnsupportedFileType { path } => write!(
                formatter,
                "controller source {} is not a regular file, FIFO, or TTY",
                path.display()
            ),
            Self::Gate0RequiresTty { path } => write!(
                formatter,
                "Gate 0A controller source {} is not a TTY",
                path.display()
            ),
            Self::RawDisciplineReadbackMismatch { path } => write!(
                formatter,
                "controller source {} did not retain the required raw TTY discipline",
                path.display()
            ),
        }
    }
}

impl Error for ControllerSourceError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::CharacterDeviceIsNotTty { .. }
            | Self::UnsupportedFileType { .. }
            | Self::Gate0RequiresTty { .. }
            | Self::RawDisciplineReadbackMismatch { .. } => None,
        }
    }
}

/// Open one controller source and establish its byte-stream discipline.
///
/// Regular files and FIFOs are useful deterministic fixtures and receive no
/// termios calls. A character device must be a real TTY. TTY input is made raw
/// because canonical editing, CR/LF conversion, echo, signal characters, or
/// XON/XOFF would silently change binary COBS frames.
///
/// # Errors
///
/// Returns an error when the path cannot be opened or inspected, the file type
/// is unsupported, a character device is not a TTY, a termios operation fails,
/// or the effective settings differ from the required discipline.
pub fn open_controller_source(
    path: impl AsRef<Path>,
) -> Result<OpenedControllerSource, ControllerSourceError> {
    let path = path.as_ref().to_path_buf();
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOCTTY)
        .open(&path)
        .map_err(|source| io_failure(&path, ControllerSourceOperation::Open, source))?;
    let file_type = file
        .metadata()
        .map_err(|source| io_failure(&path, ControllerSourceOperation::InspectFileType, source))?
        .file_type();

    let (kind, discipline) = if file_type.is_file() {
        (
            ControllerSourceKind::RegularFile,
            ControllerSourceDiscipline::PassThrough,
        )
    } else if file_type.is_fifo() {
        (
            ControllerSourceKind::Fifo,
            ControllerSourceDiscipline::PassThrough,
        )
    } else if file_type.is_char_device() {
        if !file.is_terminal() {
            return Err(ControllerSourceError::CharacterDeviceIsNotTty { path });
        }
        configure_and_verify_raw_tty(&path, file.as_raw_fd())?;
        (
            ControllerSourceKind::Tty,
            ControllerSourceDiscipline::RawTty(RAW_CONTROLLER_TTY_DISCIPLINE),
        )
    } else {
        return Err(ControllerSourceError::UnsupportedFileType { path });
    };

    Ok(OpenedControllerSource {
        report: ControllerSourceReport {
            path,
            kind,
            discipline,
        },
        file,
    })
}

/// Open the one Gate 0A controller as an exclusive duplex TTY.
///
/// Unlike [`open_controller_source`], this path rejects deterministic file and
/// FIFO fixtures. Gate 0A must prove the real serial-line ownership and duplex
/// semantics that its stop request relies on. Hardware-independent tests use a
/// real PTY, which exercises the same kernel terminal APIs.
///
/// # Errors
///
/// Returns an error unless `path` is a TTY that can be opened read/write,
/// exclusively claimed with `TIOCEXCL`, configured as raw 921600 8N1, and
/// duplicated for independent reader and writer ownership.
pub fn open_gate0_controller(
    path: impl AsRef<Path>,
) -> Result<OpenedGate0Controller, ControllerSourceError> {
    let path = path.as_ref().to_path_buf();
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOCTTY)
        .open(&path)
        .map_err(|source| io_failure(&path, ControllerSourceOperation::Open, source))?;
    let file_type = file
        .metadata()
        .map_err(|source| io_failure(&path, ControllerSourceOperation::InspectFileType, source))?
        .file_type();
    if !file_type.is_char_device() || !file.is_terminal() {
        return Err(if file_type.is_char_device() {
            ControllerSourceError::CharacterDeviceIsNotTty { path }
        } else {
            ControllerSourceError::Gate0RequiresTty { path }
        });
    }

    acquire_exclusive_access(&path, file.as_raw_fd())?;
    configure_and_verify_raw_tty(&path, file.as_raw_fd())?;
    let writer = file.try_clone().map_err(|source| {
        io_failure(
            &path,
            ControllerSourceOperation::DuplicateDuplexDescriptor,
            source,
        )
    })?;
    Ok(OpenedGate0Controller {
        reader: file,
        writer: Gate0ControllerWriter {
            file: writer,
            path: path.clone(),
        },
        report: ControllerSourceReport {
            path,
            kind: ControllerSourceKind::Tty,
            discipline: ControllerSourceDiscipline::RawTty(RAW_CONTROLLER_TTY_DISCIPLINE),
        },
    })
}

fn configure_and_verify_raw_tty(
    path: &Path,
    file_descriptor: RawFd,
) -> Result<(), ControllerSourceError> {
    let mut settings = read_termios(
        path,
        file_descriptor,
        ControllerSourceOperation::ReadTermios,
    )?;

    // `cfmakeraw` establishes the broad POSIX raw baseline. The explicit flag
    // edits below pin the binary framing requirements that matter to this wire.
    make_raw(&mut settings);
    settings.c_iflag &= !(libc::IXON | libc::IXOFF | libc::IXANY);
    settings.c_cflag &= !(libc::CSIZE | libc::PARENB | libc::CSTOPB | libc::CRTSCTS);
    settings.c_cflag |= libc::CS8 | libc::CREAD | libc::CLOCAL;
    settings.c_cc[libc::VMIN] = RAW_CONTROLLER_TTY_DISCIPLINE.minimum_read_bytes;
    settings.c_cc[libc::VTIME] = RAW_CONTROLLER_TTY_DISCIPLINE.read_timeout_deciseconds;

    set_input_speed(path, &mut settings)?;
    set_output_speed(path, &mut settings)?;
    apply_termios(path, file_descriptor, &settings)?;

    // `tcsetattr` success proves only that the driver accepted the request.
    // Readback catches drivers that normalize or omit settings, before any
    // acquisition thread can treat a transformed stream as sensor evidence.
    let effective = read_termios(
        path,
        file_descriptor,
        ControllerSourceOperation::ReadBackTermios,
    )?;
    if !is_required_raw_discipline(&effective) {
        return Err(ControllerSourceError::RawDisciplineReadbackMismatch {
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

fn io_failure(
    path: &Path,
    operation: ControllerSourceOperation,
    source: io::Error,
) -> ControllerSourceError {
    ControllerSourceError::Io {
        path: path.to_path_buf(),
        operation,
        source,
    }
}

#[allow(unsafe_code)]
fn acquire_exclusive_access(
    path: &Path,
    file_descriptor: RawFd,
) -> Result<(), ControllerSourceError> {
    // SAFETY: the descriptor belongs to a live TTY `File`. `TIOCEXCL` changes
    // only terminal-open policy and neither retains nor closes the descriptor.
    if unsafe { libc::ioctl(file_descriptor, libc::TIOCEXCL) } != 0 {
        return Err(io_failure(
            path,
            ControllerSourceOperation::AcquireExclusiveAccess,
            io::Error::last_os_error(),
        ));
    }
    Ok(())
}

#[allow(unsafe_code)]
fn drain_output(path: &Path, file_descriptor: RawFd) -> Result<(), ControllerSourceError> {
    // SAFETY: the descriptor belongs to the live Gate 0A writer. `tcdrain`
    // blocks until queued terminal output is transmitted and retains nothing.
    if unsafe { libc::tcdrain(file_descriptor) } != 0 {
        return Err(io_failure(
            path,
            ControllerSourceOperation::DrainOutput,
            io::Error::last_os_error(),
        ));
    }
    Ok(())
}

fn validate_pipe_descriptor(
    file: &File,
    end: Gate0ControlPipeEnd,
) -> Result<(), Gate0ControlPipeError> {
    let metadata = file
        .metadata()
        .map_err(|source| Gate0ControlPipeError::Io {
            end,
            operation: "inspect",
            source,
        })?;
    if !metadata.file_type().is_fifo() {
        return Err(Gate0ControlPipeError::NotPipe(end));
    }
    let flags = descriptor_flags(file, end)?;
    let required_mode = match end {
        Gate0ControlPipeEnd::ReadyWrite => libc::O_WRONLY,
        Gate0ControlPipeEnd::StopRead => libc::O_RDONLY,
    };
    if flags & libc::O_ACCMODE != required_mode {
        return Err(Gate0ControlPipeError::WrongAccessMode(end));
    }
    Ok(())
}

#[allow(unsafe_code)]
fn descriptor_flags(
    file: &File,
    end: Gate0ControlPipeEnd,
) -> Result<libc::c_int, Gate0ControlPipeError> {
    // SAFETY: `F_GETFL` only inspects the live descriptor and retains nothing.
    let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
    if flags < 0 {
        return Err(Gate0ControlPipeError::Io {
            end,
            operation: "read status flags",
            source: io::Error::last_os_error(),
        });
    }
    Ok(flags)
}

#[allow(unsafe_code)]
fn set_close_on_exec(file: &File, end: Gate0ControlPipeEnd) -> Result<(), Gate0ControlPipeError> {
    // SAFETY: both `fcntl` calls operate on the live descriptor and retain no
    // pointers. The first obtains descriptor flags and the second adds CLOEXEC.
    let descriptor_flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFD) };
    if descriptor_flags < 0
        || unsafe {
            libc::fcntl(
                file.as_raw_fd(),
                libc::F_SETFD,
                descriptor_flags | libc::FD_CLOEXEC,
            )
        } < 0
    {
        return Err(Gate0ControlPipeError::Io {
            end,
            operation: "set close-on-exec",
            source: io::Error::last_os_error(),
        });
    }
    Ok(())
}

#[allow(unsafe_code)]
fn set_nonblocking(file: &File, end: Gate0ControlPipeEnd) -> Result<(), Gate0ControlPipeError> {
    let flags = descriptor_flags(file, end)?;
    // SAFETY: `F_SETFL` updates status flags on the live descriptor and retains
    // neither the descriptor nor any pointer.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(Gate0ControlPipeError::Io {
            end,
            operation: "set nonblocking mode",
            source: io::Error::last_os_error(),
        });
    }
    Ok(())
}

#[allow(unsafe_code)]
fn read_termios(
    path: &Path,
    file_descriptor: RawFd,
    operation: ControllerSourceOperation,
) -> Result<libc::termios, ControllerSourceError> {
    let mut settings = MaybeUninit::<libc::termios>::uninit();
    // SAFETY: `settings` points to writable storage for one `termios` value and
    // the live descriptor was proven to be a TTY before this helper is called.
    if unsafe { libc::tcgetattr(file_descriptor, settings.as_mut_ptr()) } != 0 {
        return Err(io_failure(path, operation, io::Error::last_os_error()));
    }
    // SAFETY: a successful `tcgetattr` initialized the entire `termios` value.
    Ok(unsafe { settings.assume_init() })
}

#[allow(unsafe_code)]
fn make_raw(settings: &mut libc::termios) {
    // SAFETY: the mutable reference identifies an initialized `termios` value.
    unsafe { libc::cfmakeraw(settings) };
}

#[allow(unsafe_code)]
fn set_input_speed(path: &Path, settings: &mut libc::termios) -> Result<(), ControllerSourceError> {
    // SAFETY: the mutable reference identifies an initialized `termios` value.
    if unsafe { libc::cfsetispeed(settings, libc::B921600) } != 0 {
        return Err(io_failure(
            path,
            ControllerSourceOperation::SetInputSpeed,
            io::Error::last_os_error(),
        ));
    }
    Ok(())
}

#[allow(unsafe_code)]
fn set_output_speed(
    path: &Path,
    settings: &mut libc::termios,
) -> Result<(), ControllerSourceError> {
    // SAFETY: the mutable reference identifies an initialized `termios` value.
    if unsafe { libc::cfsetospeed(settings, libc::B921600) } != 0 {
        return Err(io_failure(
            path,
            ControllerSourceOperation::SetOutputSpeed,
            io::Error::last_os_error(),
        ));
    }
    Ok(())
}

#[allow(unsafe_code)]
fn apply_termios(
    path: &Path,
    file_descriptor: RawFd,
    settings: &libc::termios,
) -> Result<(), ControllerSourceError> {
    // SAFETY: `settings` is initialized and the live descriptor is a TTY.
    if unsafe { libc::tcsetattr(file_descriptor, libc::TCSANOW, settings) } != 0 {
        return Err(io_failure(
            path,
            ControllerSourceOperation::ApplyTermios,
            io::Error::last_os_error(),
        ));
    }
    Ok(())
}

fn is_required_raw_discipline(settings: &libc::termios) -> bool {
    let transformed_input = libc::IGNBRK
        | libc::BRKINT
        | libc::PARMRK
        | libc::ISTRIP
        | libc::INLCR
        | libc::IGNCR
        | libc::ICRNL
        | libc::IXON
        | libc::IXOFF
        | libc::IXANY;
    let transformed_local = libc::ECHO | libc::ECHONL | libc::ICANON | libc::ISIG | libc::IEXTEN;

    settings.c_iflag & transformed_input == 0
        && settings.c_oflag & libc::OPOST == 0
        && settings.c_lflag & transformed_local == 0
        && settings.c_cflag & libc::CSIZE == libc::CS8
        && settings.c_cflag & (libc::PARENB | libc::CSTOPB | libc::CRTSCTS) == 0
        && settings.c_cflag & (libc::CREAD | libc::CLOCAL) == (libc::CREAD | libc::CLOCAL)
        && settings.c_cc[libc::VMIN] == RAW_CONTROLLER_TTY_DISCIPLINE.minimum_read_bytes
        && settings.c_cc[libc::VTIME] == RAW_CONTROLLER_TTY_DISCIPLINE.read_timeout_deciseconds
        && terminal_speeds_match(settings)
}

#[allow(unsafe_code)]
fn terminal_speeds_match(settings: &libc::termios) -> bool {
    // SAFETY: both calls inspect the initialized value without retaining it.
    unsafe {
        libc::cfgetispeed(settings) == libc::B921600 && libc::cfgetospeed(settings) == libc::B921600
    }
}
