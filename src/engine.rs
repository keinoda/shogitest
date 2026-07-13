use crate::shogi;
use log::{error, info, trace};
#[cfg(target_os = "linux")]
use std::os::unix::process::CommandExt;
use std::{
    io::{Result, Write},
    path::Path,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    time::Duration,
};
use wait_timeout::ChildExt;

#[derive(Debug, Clone, Default)]
pub enum Score {
    #[default]
    None,
    Cp(i32),
    Mate(i32),
}

#[derive(Debug)]
pub enum EngineResult<T> {
    Ok(T),
    Timeout,
    Disconnected,
    Err(std::io::Error),
}

#[derive(Debug, Copy, Clone)]
pub enum ReadState {
    Continue,
    Stop,
}

#[derive(Debug, Clone, Default)]
pub struct MoveRecord {
    pub stm: Option<shogi::Color>,
    pub m: shogi::Move,
    pub mstr: String,
    pub ponder: Option<shogi::Move>,
    pub score: Score,
    pub depth: u32,
    pub seldepth: u32,
    pub nodes: u64,
    pub nps: u64,
    pub engine_time: u64,
    pub hashfull: u32,
    pub measured_time: Duration,
    pub time_left: Option<Duration>,
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct EngineBuilder {
    pub dir: String,
    pub cmd: String,
    pub name: Option<String>,
    pub usi_options: Vec<(String, String)>,
}

impl EngineBuilder {
    pub fn init(&self) -> Result<Engine> {
        self.init_with_affinity(None)
    }

    pub fn init_with_affinity(&self, cpu_affinity: Option<&[usize]>) -> Result<Engine> {
        let cmd = if self.dir.is_empty() {
            Path::new(&self.cmd).to_path_buf()
        } else {
            Path::new(&self.dir).join(&self.cmd)
        };

        let mut command = Command::new(&cmd);
        command.stdout(Stdio::piped()).stdin(Stdio::piped());
        if let Some(cpus) = cpu_affinity {
            configure_cpu_affinity(&mut command, cpus)?;
        }
        let mut child = command.spawn().map_err(|err| {
            std::io::Error::new(
                err.kind(),
                format!(
                    "Failed to start engine {} with CPU affinity {:?}: {err}",
                    cmd.display(),
                    cpu_affinity
                ),
            )
        })?;

        let stdout = child.stdout.take().unwrap();
        let stdin = child.stdin.take().unwrap();

        let mut engine = Engine {
            child,
            stdout,
            read_buf: Vec::new(),
            stdin,
            name: self.name.clone().unwrap_or(self.cmd.to_string()),
            builder: self.clone(),
            cpu_affinity: cpu_affinity.map(<[_]>::to_vec),
        };

        engine.write_line("usi")?;

        let mut usi_name: Option<String> = None;
        match engine.read_with_timeout(Some(5 * Duration::SECOND), |line| {
            let mut it = line.split_whitespace();
            match it.next() {
                Some("usiok") => ReadState::Stop,
                Some("id") => {
                    match it.next() {
                        Some("name") => {
                            if let Some(name) = it.remainder() {
                                usi_name = Some(name.trim().to_string());
                            }
                        }
                        Some("author") => {}
                        s => {
                            dbg!(s);
                        }
                    }
                    ReadState::Continue
                }
                _ => ReadState::Continue,
            }
        }) {
            EngineResult::Ok(()) => {}
            EngineResult::Err(err) => return Err(err),
            EngineResult::Timeout => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("Timed-out waiting for usiok for {}", engine.name),
                ));
            }
            EngineResult::Disconnected => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    format!(
                        "Engine {} disconnected while waiting for usiok",
                        engine.name
                    ),
                ));
            }
        }

        if let Some(usi_name) = usi_name
            && self.name.is_none()
        {
            engine.name = usi_name;
        }

        for (k, v) in &self.usi_options {
            engine.write_line(&format!("setoption name {k} value {v}"))?;
        }

        info!("Engine {} started", engine.name);

        Ok(engine)
    }
    pub fn get_usi_option_value(&self, key: &str) -> Option<&str> {
        self.usi_options
            .iter()
            .filter_map(|(k, v)| if k == key { Some(v.as_ref()) } else { None })
            .next_back()
    }
}

#[cfg(target_os = "linux")]
fn configure_cpu_affinity(command: &mut Command, cpus: &[usize]) -> Result<()> {
    if cpus.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Engine CPU affinity must not be empty",
        ));
    }
    if let Some(cpu) = cpus
        .iter()
        .copied()
        .find(|cpu| *cpu >= libc::CPU_SETSIZE as usize)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("CPU {cpu} exceeds Linux CPU_SETSIZE {}", libc::CPU_SETSIZE),
        ));
    }

    let cpus = cpus.to_vec();
    unsafe {
        command.pre_exec(move || {
            let mut set: libc::cpu_set_t = std::mem::zeroed();
            libc::CPU_ZERO(&mut set);
            for cpu in &cpus {
                libc::CPU_SET(*cpu, &mut set);
            }
            if libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn configure_cpu_affinity(_command: &mut Command, _cpus: &[usize]) -> Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "Engine CPU affinity is supported only on Linux",
    ))
}

#[derive(Debug)]
pub struct Engine {
    child: Child,
    stdout: ChildStdout,
    read_buf: Vec<u8>,
    stdin: ChildStdin,
    name: String,
    builder: EngineBuilder,
    cpu_affinity: Option<Vec<usize>>,
}

impl Drop for Engine {
    fn drop(&mut self) {
        info!("Quitting engine {}...", self.name);
        match self.write_line("quit") {
            Ok(_) => {}
            Err(_) => error!("Failed to write quit to engine {}", self.name),
        };
        match self.child.wait_timeout(Duration::from_secs(10)) {
            Ok(Some(_)) => info!("Quit engine {} successfully", self.name),
            Ok(None) | Err(_) => {
                info!(
                    "Timed out quitting engine {}, attempting to kill...",
                    self.name
                );
                match self.child.kill() {
                    Ok(_) => info!("Engine {} killed", self.name),
                    Err(_) => info!("Failed to kill engine {}, giving up", self.name),
                }
            }
        }
    }
}

impl Engine {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn restart(&mut self) -> Result<()> {
        let builder = self.builder.clone();
        let cpu_affinity = self.cpu_affinity.clone();
        *self = builder.init_with_affinity(cpu_affinity.as_deref())?;
        Ok(())
    }

    pub fn write_line(&mut self, line: &str) -> Result<()> {
        trace!("{} < {line}", self.name());
        writeln!(self.stdin, "{line}")
    }

    pub fn isready(&mut self) -> Result<()> {
        self.write_line("isready")?;
        self.flush()?;
        match self.read_with_timeout(Some(5 * Duration::SECOND), |line| {
            if line.trim().eq_ignore_ascii_case("readyok") {
                ReadState::Stop
            } else {
                ReadState::Continue
            }
        }) {
            EngineResult::Ok(()) => Ok(()),
            EngineResult::Err(err) => Err(err),
            EngineResult::Timeout => Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("Timed-out waiting for readyok for {}", self.name),
            )),
            EngineResult::Disconnected => Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                format!(
                    "Engine {} disconnected while waiting for readyok",
                    self.name
                ),
            )),
        }
    }

    pub fn usinewgame(&mut self) -> Result<()> {
        self.write_line("usinewgame")?;
        self.flush()?;
        Ok(())
    }

    pub fn position(&mut self, game: &shogi::Game) -> Result<()> {
        let position = format!("position {}", game.usi_string());
        self.write_line(&position)?;
        self.flush()?;
        Ok(())
    }

    pub fn wait_for_bestmove(
        &mut self,
        stm: crate::shogi::Color,
        timeout: Option<Duration>,
    ) -> EngineResult<MoveRecord> {
        let mut mr = MoveRecord {
            stm: Some(stm),
            ..MoveRecord::default()
        };
        match self.read_with_timeout(timeout, |line| parse_search_output_line(&line, &mut mr)) {
            EngineResult::Ok(()) => EngineResult::Ok(mr),
            EngineResult::Err(err) => EngineResult::Err(err),
            EngineResult::Timeout => EngineResult::Timeout,
            EngineResult::Disconnected => EngineResult::Disconnected,
        }
    }

    pub fn flush(&mut self) -> Result<()> {
        self.stdin.flush()
    }

    #[cfg(unix)]
    pub fn read_with_timeout<F>(&mut self, timeout: Option<Duration>, mut f: F) -> EngineResult<()>
    where
        F: FnMut(String) -> ReadState,
    {
        use std::io::Read;
        use std::os::fd::AsRawFd;

        let timeout_ms = match timeout {
            Some(timeout) => timeout.as_millis().clamp(0, i32::MAX as u128) as i32,
            None => -1,
        };

        loop {
            let mut fds: [libc::pollfd; 1] = unsafe { std::mem::zeroed() };
            fds[0].fd = self.stdout.as_raw_fd();
            fds[0].events = libc::POLLIN;

            let ready_count =
                unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout_ms) };
            if ready_count < 0 {
                let err = std::io::Error::last_os_error();
                match err.raw_os_error() {
                    Some(libc::EINTR) | Some(libc::EAGAIN) => continue,
                    _ => return EngineResult::Err(err),
                }
            }

            assert!(ready_count as usize <= fds.len());

            if ready_count == 0 {
                return EngineResult::Timeout;
            }

            let count = {
                self.read_buf.reserve(4096);
                let old_len = self.read_buf.len();
                let spare_cap = self.read_buf.spare_capacity_mut();
                let spare_cap = unsafe {
                    std::slice::from_raw_parts_mut(
                        spare_cap.as_mut_ptr() as *mut u8,
                        spare_cap.len(),
                    )
                };
                match self.stdout.read(spare_cap) {
                    Err(err) => return EngineResult::Err(err),
                    Ok(count) => {
                        unsafe { self.read_buf.set_len(old_len + count) };
                        count
                    }
                }
            };

            if count == 0 {
                return EngineResult::Disconnected;
            }

            match self.process_read_buf(&mut f) {
                Ok(ReadState::Continue) => {}
                Ok(ReadState::Stop) => return EngineResult::Ok(()),
                Err(err) => return EngineResult::Err(err),
            }
        }
    }

    #[cfg(windows)]
    pub fn read_with_timeout<F>(&mut self, timeout: Option<Duration>, mut f: F) -> EngineResult<()>
    where
        F: FnMut(String) -> ReadState,
    {
        use std::os::windows::io::AsRawHandle;
        use windows::{
            Win32::Foundation::*, Win32::Storage::FileSystem::*, Win32::System::IO::*,
            Win32::System::Threading::*,
        };

        let timeout_ms = match timeout {
            Some(timeout) => timeout.as_millis().clamp(0, i32::MAX as u128) as u32,
            None => INFINITE,
        };

        loop {
            unsafe {
                let handle = HANDLE(self.stdout.as_raw_handle());

                let mut overlapped = OVERLAPPED::default();
                overlapped.hEvent =
                    CreateEventW(None, true, false, None).expect("Could not create event");

                let old_read_buf_len = self.read_buf.len();

                let write_buf = {
                    self.read_buf.reserve(4096);
                    let spare_cap = self.read_buf.spare_capacity_mut();
                    std::slice::from_raw_parts_mut(
                        spare_cap.as_mut_ptr() as *mut u8,
                        spare_cap.len(),
                    )
                };

                if let Err(err) = ReadFile(handle, Some(write_buf), None, Some(&mut overlapped))
                    && err.code() != ERROR_IO_PENDING.into()
                {
                    let _ = CloseHandle(overlapped.hEvent);
                    return EngineResult::Err(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        format!("ReadFile Failed: {:?}", err),
                    ));
                }

                match WaitForSingleObject(overlapped.hEvent, timeout_ms) {
                    WAIT_TIMEOUT => {
                        let _ = CancelIo(handle);
                        let _ = CloseHandle(overlapped.hEvent);
                        return EngineResult::Timeout;
                    }
                    WAIT_OBJECT_0 => {}
                    _ => {
                        let _ = CloseHandle(overlapped.hEvent);
                        return EngineResult::Err(std::io::Error::new(
                            std::io::ErrorKind::Other,
                            "WaitForSingleObject Failed",
                        ));
                    }
                }

                let mut bytes_read: u32 = 0;
                if let Err(err) = GetOverlappedResult(handle, &overlapped, &mut bytes_read, false) {
                    let _ = CloseHandle(overlapped.hEvent);
                    return EngineResult::Err(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        format!("GetOverlappedResult Failed: {:?}", err),
                    ));
                }

                let _ = CloseHandle(overlapped.hEvent);

                self.read_buf
                    .set_len(old_read_buf_len + bytes_read as usize);

                if bytes_read == 0 {
                    return EngineResult::Disconnected;
                }

                match self.process_read_buf(&mut f) {
                    Ok(ReadState::Continue) => {}
                    Ok(ReadState::Stop) => return EngineResult::Ok(()),
                    Err(err) => return EngineResult::Err(err),
                }
            }
        }
    }

    fn process_read_buf<F>(&mut self, mut f: F) -> Result<ReadState>
    where
        F: FnMut(String) -> ReadState,
    {
        while let Some(i) = memchr::memchr(b'\n', self.read_buf.as_slice()) {
            let line = {
                let line = self.read_buf.drain(0..(i + 1));
                let Ok(line) = str::from_utf8(line.as_slice()) else {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "Received Invalid UTF-8",
                    ));
                };
                line.to_string()
            };

            trace!("{} > {}", self.name(), line.trim());

            match f(line) {
                ReadState::Continue => {}
                ReadState::Stop => return Ok(ReadState::Stop),
            }
        }

        Ok(ReadState::Continue)
    }
}

fn parse_search_output_line(line: &str, mr: &mut MoveRecord) -> ReadState {
    let mut it = line.split_ascii_whitespace();
    match it.next() {
        Some("info") => {
            while let Some(tok) = it.next() {
                match tok {
                    "string" => break,
                    "depth" => {
                        if let Some(value) = it.next()
                            && let Ok(value) = value.parse::<u32>()
                        {
                            mr.depth = value;
                        }
                    }
                    "seldepth" => {
                        if let Some(value) = it.next()
                            && let Ok(value) = value.parse::<u32>()
                        {
                            mr.seldepth = value;
                        }
                    }
                    "nodes" => {
                        if let Some(value) = it.next()
                            && let Ok(value) = value.parse::<u64>()
                        {
                            mr.nodes = value;
                        }
                    }
                    "nps" => {
                        if let Some(value) = it.next()
                            && let Ok(value) = value.parse::<u64>()
                        {
                            mr.nps = value;
                        }
                    }
                    "time" => {
                        if let Some(value) = it.next()
                            && let Ok(value) = value.parse::<u64>()
                        {
                            mr.engine_time = value;
                        }
                    }
                    "hashfull" => {
                        if let Some(value) = it.next()
                            && let Ok(value) = value.parse::<u32>()
                        {
                            mr.hashfull = value;
                        }
                    }
                    "score" => match it.next() {
                        Some("cp") => {
                            if let Some(value) = it.next()
                                && let Ok(value) = value.parse::<i32>()
                            {
                                mr.score = Score::Cp(value);
                            }
                        }
                        Some("mate") => {
                            if let Some(value) = it.next()
                                && let Ok(value) = value.parse::<i32>()
                            {
                                mr.score = Score::Mate(value);
                            }
                        }
                        _ => continue,
                    },
                    _ => continue,
                }
            }
            ReadState::Continue
        }
        Some("bestmove") => {
            let mstr = it.next().unwrap_or("");
            mr.mstr = mstr.to_string();
            if let Some(m) = shogi::Move::parse(mstr) {
                mr.m = m;
            }

            while let Some(token) = it.next() {
                if token == "ponder" {
                    mr.ponder = it.next().and_then(shogi::Move::parse).filter(|m| {
                        matches!(m, shogi::Move::Normal { .. } | shogi::Move::Drop(_, _))
                    });
                    break;
                }
            }
            ReadState::Stop
        }
        _ => ReadState::Continue,
    }
}

#[cfg(all(test, target_os = "linux"))]
mod affinity_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn first_allowed_cpu() -> usize {
        let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
        let result =
            unsafe { libc::sched_getaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &mut set) };
        assert_eq!(result, 0);
        (0..libc::CPU_SETSIZE as usize)
            .find(|cpu| unsafe { libc::CPU_ISSET(*cpu, &set) })
            .expect("test process has no allowed CPU")
    }

    #[test]
    fn pins_child_before_exec() {
        let cpu = first_allowed_cpu();
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "grep '^Cpus_allowed_list:' /proc/self/status"]);
        configure_cpu_affinity(&mut command, &[cpu]).unwrap();

        let output = command.output().unwrap();
        assert!(output.status.success());
        let stdout = String::from_utf8(output.stdout).unwrap();
        let actual = stdout.split_once(':').unwrap().1.trim();
        assert_eq!(actual, cpu.to_string());
    }

    fn process_allowed_list(pid: u32) -> String {
        let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
        status
            .lines()
            .find_map(|line| line.strip_prefix("Cpus_allowed_list:"))
            .unwrap()
            .trim()
            .to_string()
    }

    #[test]
    fn preserves_affinity_when_restarting_an_engine() {
        let cpu = first_allowed_cpu();
        let script_path = std::env::temp_dir().join(format!(
            "shogitest-affinity-engine-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        std::fs::write(
            &script_path,
            "#!/bin/sh\nwhile IFS= read -r line; do\n  case \"$line\" in\n    usi) echo 'id name affinity-test'; echo usiok ;;\n    quit) exit 0 ;;\n  esac\ndone\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&script_path).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&script_path, permissions).unwrap();

        let builder = EngineBuilder {
            cmd: script_path.to_string_lossy().into_owned(),
            ..EngineBuilder::default()
        };
        let mut engine = builder.init_with_affinity(Some(&[cpu])).unwrap();
        let first_pid = engine.child.id();
        assert_eq!(process_allowed_list(first_pid), cpu.to_string());

        engine.restart().unwrap();
        assert_ne!(engine.child.id(), first_pid);
        assert_eq!(process_allowed_list(engine.child.id()), cpu.to_string());

        drop(engine);
        std::fs::remove_file(script_path).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bestmove_and_ponder_move() {
        let mut record = MoveRecord::default();

        assert!(matches!(
            parse_search_output_line("bestmove 7g7f ponder 3c3d", &mut record),
            ReadState::Stop
        ));
        assert_eq!(record.m, shogi::Move::parse("7g7f").unwrap());
        assert_eq!(record.ponder, shogi::Move::parse("3c3d"));
    }

    #[test]
    fn ignores_unusable_ponder_move() {
        let mut record = MoveRecord::default();

        parse_search_output_line("bestmove 7g7f ponder resign", &mut record);

        assert_eq!(record.ponder, None);
    }
}
