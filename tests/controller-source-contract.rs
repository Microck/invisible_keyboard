#![cfg(target_os = "linux")]

use std::{
    ffi::{CStr, CString},
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    mem::MaybeUninit,
    os::{
        fd::{AsRawFd, FromRawFd, IntoRawFd, RawFd},
        unix::{ffi::OsStrExt, fs::OpenOptionsExt},
    },
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    thread,
};

use invisible_keyboard::controller_source::{
    ControllerSourceDiscipline, ControllerSourceError, ControllerSourceKind, ControllerTtyParity,
    Gate0ControlPipeError, Gate0ControlPipes, Gate0StopPipeRead, RAW_CONTROLLER_TTY_DISCIPLINE,
    open_controller_source, open_gate0_controller,
};

static NEXT_FIXTURE_ID: AtomicU64 = AtomicU64::new(0);

#[test]
fn regular_file_is_an_untouched_byte_stream() {
    let fixture = TemporaryPath::new("regular");
    let expected = [0_u8, 0x11, b'\r', b'\n', 0x13, 0xff];
    fs::write(fixture.path(), expected).expect("write regular controller fixture");

    let mut source =
        open_controller_source(fixture.path()).expect("open regular controller source");
    assert_eq!(source.report.path.as_path(), fixture.path());
    assert_eq!(source.report.kind, ControllerSourceKind::RegularFile);
    assert_eq!(
        source.report.discipline,
        ControllerSourceDiscipline::PassThrough
    );

    let mut actual = Vec::new();
    source
        .file
        .read_to_end(&mut actual)
        .expect("read regular controller source");
    assert_eq!(actual, expected);
}

#[test]
fn fifo_is_an_untouched_byte_stream() {
    let fixture = TemporaryPath::new("fifo");
    create_fifo(fixture.path()).expect("create controller FIFO");
    let writer_path = fixture.path().to_path_buf();
    let expected = [0_u8, 0x11, b'\r', b'\n', 0x13, 0xff];
    let writer = thread::spawn(move || {
        let mut fifo = OpenOptions::new()
            .write(true)
            .open(writer_path)
            .expect("open controller FIFO writer");
        fifo.write_all(&expected)
            .expect("write controller FIFO fixture");
    });

    let mut source = open_controller_source(fixture.path()).expect("open FIFO controller source");
    assert_eq!(source.report.kind, ControllerSourceKind::Fifo);
    assert_eq!(
        source.report.discipline,
        ControllerSourceDiscipline::PassThrough
    );
    let mut actual = [0_u8; 6];
    source
        .file
        .read_exact(&mut actual)
        .expect("read controller FIFO");
    writer.join().expect("controller FIFO writer joins");
    assert_eq!(actual, expected);
}

#[test]
fn character_device_that_is_not_a_tty_fails_closed() {
    let error = open_controller_source("/dev/null").expect_err("/dev/null must be rejected");
    assert!(matches!(
        error,
        ControllerSourceError::CharacterDeviceIsNotTty { path }
            if path == Path::new("/dev/null")
    ));
}

#[test]
fn real_pty_is_configured_and_read_back_as_byte_exact_raw_8n1() {
    let mut pty = PseudoTerminal::open().expect("allocate real pseudo-terminal");
    install_translating_tty_discipline(&pty.slave_path)
        .expect("install non-raw PTY starting state");

    let mut source = open_controller_source(&pty.slave_path).expect("open PTY controller source");
    assert_eq!(source.report.kind, ControllerSourceKind::Tty);
    assert_eq!(
        source.report.discipline,
        ControllerSourceDiscipline::RawTty(RAW_CONTROLLER_TTY_DISCIPLINE)
    );
    assert_eq!(RAW_CONTROLLER_TTY_DISCIPLINE.baud_rate, 921_600);
    assert_eq!(RAW_CONTROLLER_TTY_DISCIPLINE.data_bits, 8);
    assert_eq!(
        RAW_CONTROLLER_TTY_DISCIPLINE.parity,
        ControllerTtyParity::None
    );
    assert_eq!(RAW_CONTROLLER_TTY_DISCIPLINE.stop_bits, 1);
    const {
        assert!(!RAW_CONTROLLER_TTY_DISCIPLINE.software_flow_control);
        assert!(!RAW_CONTROLLER_TTY_DISCIPLINE.hardware_flow_control);
    }

    let effective = read_termios(source.file.as_raw_fd()).expect("read effective PTY settings");
    assert_effective_raw_discipline(&effective);

    // These bytes exercise canonical delimiters, CR/LF translation, software
    // flow-control characters, NUL, and the high bit. All must survive exactly.
    let expected = [b'a', b'\r', b'\n', 0x11, 0x13, 0, 0x80, 0xff];
    pty.master
        .write_all(&expected)
        .expect("write binary controller bytes to PTY master");
    let mut actual = [0_u8; 8];
    source
        .file
        .read_exact(&mut actual)
        .expect("read binary controller bytes from PTY slave");
    assert_eq!(actual, expected);
}

#[test]
fn gate0_controller_is_one_exclusive_duplex_raw_tty() {
    let mut pty = PseudoTerminal::open().expect("allocate real pseudo-terminal");
    install_translating_tty_discipline(&pty.slave_path)
        .expect("install non-raw PTY starting state");

    let mut controller =
        open_gate0_controller(&pty.slave_path).expect("open exclusive Gate 0A controller");
    assert_eq!(controller.report.kind, ControllerSourceKind::Tty);
    assert_eq!(
        controller.report.discipline,
        ControllerSourceDiscipline::RawTty(RAW_CONTROLLER_TTY_DISCIPLINE)
    );
    assert_effective_raw_discipline(
        &read_termios(controller.reader.as_raw_fd()).expect("read Gate 0A PTY settings"),
    );

    let host_request = [0x11, 0, b'\r', b'\n', 0x13, 0xff];
    controller
        .writer
        .write_all(&host_request)
        .expect("write Gate 0A request bytes");
    controller
        .writer
        .drain_output()
        .expect("drain Gate 0A request bytes");
    let mut observed_request = [0_u8; 6];
    pty.master
        .read_exact(&mut observed_request)
        .expect("PTY master receives request bytes");
    assert_eq!(observed_request, host_request);

    let controller_reply = [0x13, b'\r', b'\n', 0, 0x80, 0xff];
    pty.master
        .write_all(&controller_reply)
        .expect("write Gate 0A reply bytes");
    let mut observed_reply = [0_u8; 6];
    controller
        .reader
        .read_exact(&mut observed_reply)
        .expect("Gate 0A reader receives reply bytes");
    assert_eq!(observed_reply, controller_reply);

    let second_open = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOCTTY)
        .open(&pty.slave_path)
        .expect_err("TIOCEXCL rejects a second opener");
    assert_eq!(second_open.raw_os_error(), Some(libc::EBUSY));
}

#[test]
fn gate0_controller_rejects_non_tty_sources() {
    let regular = TemporaryPath::new("gate0-regular");
    fs::write(regular.path(), b"fixture").expect("write regular fixture");
    assert!(matches!(
        open_gate0_controller(regular.path()),
        Err(ControllerSourceError::Gate0RequiresTty { .. })
    ));

    assert!(matches!(
        open_gate0_controller("/dev/null"),
        Err(ControllerSourceError::CharacterDeviceIsNotTty { .. })
    ));
}

#[test]
#[allow(unsafe_code)]
fn gate0_private_pipes_publish_once_and_preserve_stop_eof_as_a_separate_event() {
    let (ready_read, ready_write) = create_pipe().expect("create ready pipe");
    let (stop_read, mut stop_write) = create_pipe().expect("create stop pipe");
    // SAFETY: these two descriptors are distinct live ends owned by the test.
    // Ownership moves into `Gate0ControlPipes` exactly once.
    let mut pipes = unsafe {
        Gate0ControlPipes::from_inherited_descriptors(
            ready_write.into_raw_fd(),
            stop_read.into_raw_fd(),
        )
    }
    .expect("adopt correctly directed private pipes");

    assert_eq!(
        pipes.read_stop().expect("empty stop pipe reads"),
        Gate0StopPipeRead::WouldBlock
    );
    pipes.publish_ready().expect("publish readiness once");
    assert!(matches!(
        pipes.publish_ready(),
        Err(Gate0ControlPipeError::ReadinessAlreadyPublished)
    ));
    let mut ready_read = ready_read;
    let mut readiness = [0_u8; 2];
    assert_eq!(
        ready_read
            .read(&mut readiness)
            .expect("read readiness byte"),
        1
    );
    assert_eq!(readiness[0], 1);
    assert_eq!(
        ready_read
            .read(&mut readiness)
            .expect("ready EOF follows one byte"),
        0
    );

    stop_write.write_all(&[1]).expect("write sole stop event");
    assert_eq!(
        pipes.read_stop().expect("read stop event"),
        Gate0StopPipeRead::Bytes(vec![1])
    );
    assert_eq!(
        pipes.read_stop().expect("open guard has no more bytes"),
        Gate0StopPipeRead::WouldBlock
    );
    drop(stop_write);
    assert_eq!(
        pipes.read_stop().expect("closed guard reports EOF"),
        Gate0StopPipeRead::Eof
    );
}

#[test]
#[allow(unsafe_code)]
fn gate0_private_pipes_reject_aliases_standard_fds_files_and_reversed_access() {
    // No ownership transfers for these early structural rejections.
    assert!(matches!(
        // SAFETY: the function rejects both standard descriptors before adoption.
        unsafe { Gate0ControlPipes::from_inherited_descriptors(1, 0) },
        Err(Gate0ControlPipeError::StandardDescriptor)
    ));
    assert!(matches!(
        // SAFETY: the function rejects this alias before adoption.
        unsafe { Gate0ControlPipes::from_inherited_descriptors(9, 9) },
        Err(Gate0ControlPipeError::AliasedDescriptors)
    ));

    let regular = TemporaryPath::new("control-pipe-regular");
    let regular_file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(regular.path())
        .expect("open regular control fixture");
    let (stop_read, _stop_write) = create_pipe().expect("create stop pipe");
    assert!(matches!(
        // SAFETY: both live descriptors transfer exactly once for validation.
        unsafe {
            Gate0ControlPipes::from_inherited_descriptors(
                regular_file.into_raw_fd(),
                stop_read.into_raw_fd(),
            )
        },
        Err(Gate0ControlPipeError::NotPipe(_))
    ));

    let (ready_read, ready_write) = create_pipe().expect("create ready pipe");
    let (stop_read, stop_write) = create_pipe().expect("create stop pipe");
    drop(ready_write);
    drop(stop_read);
    assert!(matches!(
        // SAFETY: the live reversed ends transfer exactly once for validation.
        unsafe {
            Gate0ControlPipes::from_inherited_descriptors(
                ready_read.into_raw_fd(),
                stop_write.into_raw_fd(),
            )
        },
        Err(Gate0ControlPipeError::WrongAccessMode(_))
    ));
}

struct TemporaryPath(PathBuf);

impl TemporaryPath {
    fn new(label: &str) -> Self {
        let ordinal = NEXT_FIXTURE_ID.fetch_add(1, Ordering::Relaxed);
        Self(std::env::temp_dir().join(format!(
            "invisible-keyboard-controller-source-{}-{ordinal}-{label}",
            std::process::id()
        )))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TemporaryPath {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

struct PseudoTerminal {
    master: File,
    slave_path: PathBuf,
}

#[allow(unsafe_code)]
fn create_pipe() -> io::Result<(File, File)> {
    let mut descriptors = [-1; 2];
    // SAFETY: `descriptors` provides writable storage for both returned FDs.
    if unsafe { libc::pipe2(descriptors.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful `pipe2` returned two new independently owned FDs.
    let read = unsafe { File::from_raw_fd(descriptors[0]) };
    // SAFETY: as above, this is the distinct write end.
    let write = unsafe { File::from_raw_fd(descriptors[1]) };
    Ok((read, write))
}

impl PseudoTerminal {
    #[allow(unsafe_code)]
    fn open() -> io::Result<Self> {
        // SAFETY: `posix_openpt` takes only value arguments and returns a newly
        // owned descriptor on success.
        let master_descriptor =
            unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC) };
        if master_descriptor < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the successful `posix_openpt` result is uniquely owned here.
        let master = unsafe { File::from_raw_fd(master_descriptor) };

        // SAFETY: both calls borrow the live master descriptor and retain
        // neither the descriptor nor a pointer.
        if unsafe { libc::grantpt(master.as_raw_fd()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the same live PTY master remains owned by `master`.
        if unsafe { libc::unlockpt(master.as_raw_fd()) } != 0 {
            return Err(io::Error::last_os_error());
        }

        let mut slave_name = [0 as libc::c_char; 256];
        // SAFETY: the buffer is writable for its full declared length. On
        // success `ptsname_r` writes one NUL-terminated path into it.
        if unsafe {
            libc::ptsname_r(
                master.as_raw_fd(),
                slave_name.as_mut_ptr(),
                slave_name.len(),
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: successful `ptsname_r` produced a NUL-terminated string in
        // `slave_name`, which remains live for this conversion.
        let slave_name = unsafe { CStr::from_ptr(slave_name.as_ptr()) };
        let slave_path = PathBuf::from(std::ffi::OsStr::from_bytes(slave_name.to_bytes()));
        Ok(Self { master, slave_path })
    }
}

#[allow(unsafe_code)]
fn create_fifo(path: &Path) -> io::Result<()> {
    let path = CString::new(path.as_os_str().as_bytes())
        .expect("temporary controller FIFO path contains no NUL");
    // SAFETY: `path` is a live NUL-terminated string and mode 0o600 is valid.
    if unsafe { libc::mkfifo(path.as_ptr(), 0o600) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[allow(unsafe_code)]
fn install_translating_tty_discipline(path: &Path) -> io::Result<()> {
    let slave = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOCTTY)
        .open(path)?;
    let mut settings = read_termios(slave.as_raw_fd())?;
    settings.c_iflag |= libc::ICRNL | libc::IXON | libc::IXOFF;
    settings.c_oflag |= libc::OPOST;
    settings.c_lflag |= libc::ECHO | libc::ICANON | libc::ISIG;
    // SAFETY: `settings` is initialized and remains exclusively borrowed.
    if unsafe { libc::cfsetispeed(&mut settings, libc::B38400) } != 0
        || unsafe { libc::cfsetospeed(&mut settings, libc::B38400) } != 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the initialized settings and live PTY descriptor remain valid.
    if unsafe { libc::tcsetattr(slave.as_raw_fd(), libc::TCSANOW, &settings) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[allow(unsafe_code)]
fn read_termios(file_descriptor: RawFd) -> io::Result<libc::termios> {
    let mut settings = MaybeUninit::<libc::termios>::uninit();
    // SAFETY: `settings` provides writable storage and the caller supplies a
    // live descriptor. The test calls this helper only for real PTY slaves.
    if unsafe { libc::tcgetattr(file_descriptor, settings.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful `tcgetattr` initialized the complete value.
    Ok(unsafe { settings.assume_init() })
}

#[allow(unsafe_code)]
fn assert_effective_raw_discipline(settings: &libc::termios) {
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
    assert_eq!(settings.c_iflag & transformed_input, 0);
    assert_eq!(settings.c_oflag & libc::OPOST, 0);
    assert_eq!(settings.c_lflag & transformed_local, 0);
    assert_eq!(settings.c_cflag & libc::CSIZE, libc::CS8);
    assert_eq!(
        settings.c_cflag & (libc::PARENB | libc::CSTOPB | libc::CRTSCTS),
        0
    );
    assert_eq!(
        settings.c_cflag & (libc::CREAD | libc::CLOCAL),
        libc::CREAD | libc::CLOCAL
    );
    assert_eq!(settings.c_cc[libc::VMIN], 1);
    assert_eq!(settings.c_cc[libc::VTIME], 0);
    // SAFETY: both functions only inspect the initialized termios value.
    assert_eq!(unsafe { libc::cfgetispeed(settings) }, libc::B921600);
    // SAFETY: as above, the function retains no pointer.
    assert_eq!(unsafe { libc::cfgetospeed(settings) }, libc::B921600);
}
