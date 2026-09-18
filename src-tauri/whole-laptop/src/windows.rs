use super::{check_dns, config, preflight, CaptureProbe, Event, Request, HELPER_ARG, INTERFACE_NAME};
use std::collections::VecDeque;
use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE};
use windows_sys::Win32::Security::{
    GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
};
use windows_sys::Win32::System::Console::{
    AllocConsole, GenerateConsoleCtrlEvent, GetConsoleWindow, GetStdHandle, SetStdHandle,
    CTRL_BREAK_EVENT, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Threading::{
    CreateMutexW, GetCurrentProcess, OpenProcessToken, CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW,
    DETACHED_PROCESS,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE};

const JOURNAL: &str = "whole-laptop-owner.json";
const CONFIG: &str = "whole-laptop.json";
const START_TIMEOUT: Duration = Duration::from_secs(20);
const VERIFY_ROUTES: &str = include_str!("verify-routes.ps1");

// Get-NetAdapter -Name emits an error when the adapter does not exist yet.
// SilentlyContinue hides that error but powershell.exe -Command still exits 1.
// Enumerate and filter instead: absence is normal on the first connection,
// while a real failure to enumerate adapters must still stop startup/recovery.
const ADAPTER_QUERY: &str = "Get-NetAdapter -IncludeHidden -ErrorAction Stop | Where-Object { $_.Name -eq 'AetherWholeLaptop' }";

struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

fn wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(Some(0)).collect()
}

pub fn is_elevated() -> bool {
    unsafe {
        let mut token = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let token = Handle(token);
        let mut info: TOKEN_ELEVATION = std::mem::zeroed();
        let mut size = 0;
        GetTokenInformation(
            token.0,
            TokenElevation,
            &mut info as *mut _ as _,
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut size,
        ) != 0
            && info.TokenIsElevated != 0
    }
}

fn exclusive() -> Result<Handle, String> {
    let name = wide(OsStr::new("Global\\AetherGUI.WholeLaptop.v1"));
    unsafe {
        let handle = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
        if handle.is_null() {
            return Err(format!(
                "Cannot reserve whole-laptop routing: {}",
                std::io::Error::last_os_error()
            ));
        }
        let handle = Handle(handle);
        if GetLastError() == ERROR_ALREADY_EXISTS {
            return Err("Whole-laptop routing is already running or still stopping. Wait a few seconds, and close other Aether-GUI instances.".into());
        }
        Ok(handle)
    }
}

/// The helper and ALL its children enter one job before sing-box is launched.
/// Windows closes this deliberately process-lifetime handle on helper exit,
/// killing descendants even if the helper is forcibly terminated. It is not
/// inherited by the children. Nested jobs are supported on Windows 11.
fn supervise_children() -> Result<(), String> {
    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let job = Handle(job);
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if SetInformationJobObject(
            job.0,
            JobObjectExtendedLimitInformation,
            &limits as *const _ as _,
            std::mem::size_of_val(&limits) as u32,
        ) == 0
            || AssignProcessToJobObject(job.0, GetCurrentProcess()) == 0
        {
            return Err(format!(
                "Cannot supervise the TUN process: {}",
                std::io::Error::last_os_error()
            ));
        }
        std::mem::forget(job);
    }
    Ok(())
}

/// Give sing-box a console process group so Go receives CTRL_BREAK as an
/// interrupt and closes its adapter/routes normally. Preserve the GUI pipes:
/// AllocConsole otherwise replaces the process's standard handles.
fn hidden_console() -> Result<(), String> {
    unsafe {
        let stdin = GetStdHandle(STD_INPUT_HANDLE);
        let stdout = GetStdHandle(STD_OUTPUT_HANDLE);
        let stderr = GetStdHandle(STD_ERROR_HANDLE);
        if AllocConsole() == 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        ShowWindow(GetConsoleWindow(), SW_HIDE);
        SetStdHandle(STD_INPUT_HANDLE, stdin);
        SetStdHandle(STD_OUTPUT_HANDLE, stdout);
        SetStdHandle(STD_ERROR_HANDLE, stderr);
    }
    Ok(())
}

fn powershell(operation: &str, script: &str) -> Result<String, String> {
    let exe = PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into()))
        .join("System32/WindowsPowerShell/v1.0/powershell.exe");
    // Windows PowerShell otherwise uses the local code page for redirected
    // output. Keep localized errors readable, and do not suppress real errors.
    let script = format!(
        "$ErrorActionPreference = 'Stop'\n[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)\n{script}"
    );
    let mut child = Command::new(exe)
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-OutputFormat",
            "Text",
            "-Command",
            &script,
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Cannot run Windows network recovery during {operation}: {e}"))?;
    // Drain both pipes while waiting. A verbose error must never deadlock a
    // child while the GUI is trying to restore connectivity.
    fn capture<T: Read + Send + 'static>(mut reader: T) -> std::thread::JoinHandle<String> {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = reader.by_ref().take(16_384).read_to_end(&mut bytes);
            let _ = std::io::copy(&mut reader, &mut std::io::sink());
            // Startup/policy errors can precede our UTF-8 setting. Never lose
            // the entire message just because it contains a non-UTF-8 byte.
            String::from_utf8_lossy(&bytes).into_owned()
        })
    }
    let stdout = capture(child.stdout.take().unwrap());
    let stderr = capture(child.stderr.take().unwrap());
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let out = stdout.join().unwrap_or_default();
                let err = stderr.join().unwrap_or_default();
                return if status.success() {
                    Ok(out)
                } else {
                    let details = [err.trim(), out.trim()]
                        .into_iter()
                        .filter(|text| !text.is_empty())
                        .collect::<Vec<_>>()
                        .join("\n");
                    let details = if details.is_empty() {
                        "PowerShell returned no diagnostic output."
                    } else {
                        &details
                    };
                    Err(format!(
                        "Windows network recovery failed during {operation} ({status}): {details}"
                    ))
                };
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            other => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("Windows network recovery did not finish during {operation} ({other:?}). Use Restore normal networking, or restart Windows."));
            }
        }
    }
}

fn journal_path(data_dir: &Path) -> PathBuf {
    data_dir.join(JOURNAL)
}

fn owned(data_dir: &Path) -> Result<bool, String> {
    let path = journal_path(data_dir);
    if !path.exists() {
        return Ok(false);
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let journal: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    if journal != serde_json::json!({"interface": INTERFACE_NAME, "version": 1}) {
        return Err("Unrecognized whole-laptop recovery record; no adapters were changed.".into());
    }
    Ok(true)
}

fn cleanup(data_dir: &Path) -> Result<(), String> {
    if !owned(data_dir)? {
        return Ok(());
    }
    // Fixed interface name + ownership journal. Never reset the user's physical
    // interfaces, proxy settings, DNS servers, or routes on another adapter.
    let script = format!("$adapter = {ADAPTER_QUERY}\n")
        + r#"
if ($adapter) {
    $adapter | ForEach-Object {
        $_ | Disable-NetAdapter -Confirm:$false -ErrorAction Stop
        # Disable first so even kernel-owned routes that cannot be deleted are
        # inactive. Remove only routes belonging to our now-disabled adapter.
        Get-NetRoute -InterfaceIndex $_.ifIndex -ErrorAction SilentlyContinue | Remove-NetRoute -Confirm:$false -ErrorAction SilentlyContinue
    }
    Write-Output 'owned-adapter-retained'
}
Clear-DnsClientCache
"#;
    let remaining = powershell("adapter cleanup", &script)?;
    let _ = std::fs::remove_file(data_dir.join(CONFIG));
    // A forcibly stopped Wintun process can leave its device behind. Retain
    // ownership so the next Connect may enable/reuse just that same device.
    if !remaining.contains("owned-adapter-retained") {
        std::fs::remove_file(journal_path(data_dir)).map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub fn recover(data_dir: &Path) -> Result<(), String> {
    if !is_elevated() {
        return Err("Run Aether-GUI as administrator to restore whole-laptop networking.".into());
    }
    let _exclusive = exclusive()?;
    cleanup(data_dir)
}

fn prepare(request: &Request, probe: &CaptureProbe) -> Result<PathBuf, String> {
    cleanup(&request.data_dir)?;
    let existing = powershell(
        "adapter discovery",
        &format!("{ADAPTER_QUERY} | Select-Object -ExpandProperty Name"),
    )?;
    if existing.contains(INTERFACE_NAME) && !owned(&request.data_dir)? {
        return Err("An adapter named AetherWholeLaptop already exists without an ownership record. No adapters were changed.".into());
    }
    std::fs::create_dir_all(&request.data_dir).map_err(|e| e.to_string())?;
    // Record ownership BEFORE creating the adapter. Interrupted startup is
    // recoverable, even if sing-box never reaches its readiness log.
    if !owned(&request.data_dir)? {
        let mut journal = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(journal_path(&request.data_dir))
            .map_err(|e| e.to_string())?;
        serde_json::to_writer(
            &mut journal,
            &serde_json::json!({"interface": INTERFACE_NAME, "version": 1}),
        )
        .map_err(|e| e.to_string())?;
        journal.sync_all().map_err(|e| e.to_string())?;
    }
    if existing.contains(INTERFACE_NAME) {
        powershell("adapter activation", "Get-NetAdapter -Name 'AetherWholeLaptop' -IncludeHidden -ErrorAction Stop | Enable-NetAdapter -Confirm:$false -ErrorAction Stop")?;
    }
    let path = request.data_dir.join(CONFIG);
    let mut configuration = config(request)?;
    probe.configure(&mut configuration).map_err(|e| e.to_string())?;
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&configuration).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    Ok(path)
}

fn emit(event: Event) {
    let mut out = std::io::stdout().lock();
    let _ = serde_json::to_writer(&mut out, &event);
    let _ = out.write_all(b"\n");
    let _ = out.flush();
}

fn stop_child(child: &mut Child) {
    if matches!(child.try_wait(), Ok(Some(_))) {
        return;
    }
    unsafe {
        GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, child.id());
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn serve(request: Request, stop: mpsc::Receiver<()>) -> Result<(), String> {
    preflight(&request)?;
    let _exclusive = exclusive()?;
    supervise_children()?;
    hidden_console()?;
    let result = (|| {
        let mut probe = Some(CaptureProbe::new().map_err(|e| format!("Cannot prepare TCP capture check: {e}"))?);
        let path = prepare(&request, probe.as_ref().unwrap())?;
        if stop.try_recv().is_ok() {
            return Ok(());
        }
        let mut child = Command::new(&request.sidecar_path)
            .arg("run")
            .arg("-c")
            .arg(path)
            .current_dir(
                request
                    .sidecar_path
                    .parent()
                    .ok_or("Missing TUN binary directory")?,
            )
            .creation_flags(CREATE_NEW_PROCESS_GROUP)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Cannot start whole-laptop routing: {e}"))?;
        let ready = Arc::new(AtomicBool::new(false));
        let tail = Arc::new(Mutex::new(VecDeque::<String>::new()));
        fn read_logs<T: Read + Send + 'static>(
            reader: T,
            ready: Arc<AtomicBool>,
            tail: Arc<Mutex<VecDeque<String>>>,
        ) {
            std::thread::spawn(move || {
                let mut forwarded = 0;
                let mut traffic_forwarded = 0;
                let mut last_problem = Instant::now();
                for line in BufReader::new(reader).lines().map_while(Result::ok) {
                    if line.contains("sing-box started") {
                        ready.store(true, Ordering::Release);
                    }
                    // Include startup/initial traffic in the GUI log, then
                    // rate-limit problems so busy traffic cannot flood IPC.
                    let problem = line.contains("WARN") || line.contains("ERROR") || line.contains("FATAL");
                    // DNS bursts can consume the startup allowance. Reserve a
                    // separate allowance for actual TCP/UDP forwarding lines.
                    let traffic = line.contains("inbound connection") || line.contains("outbound connection")
                        || line.contains("inbound packet connection") || line.contains("outbound packet connection");
                    if forwarded < 24 || (traffic && traffic_forwarded < 16)
                        || (problem && last_problem.elapsed() >= Duration::from_secs(1)) {
                        emit(Event::Log(format!("[Whole laptop] {}", line.chars().take(600).collect::<String>())));
                        forwarded += 1;
                        if traffic { traffic_forwarded += 1; }
                        if problem {
                            last_problem = Instant::now();
                        }
                    }
                    let mut tail = tail.lock().unwrap();
                    if tail.len() == 6 {
                        tail.pop_front();
                    }
                    tail.push_back(line.chars().take(300).collect());
                }
            });
        }
        read_logs(child.stdout.take().unwrap(), ready.clone(), tail.clone());
        read_logs(child.stderr.take().unwrap(), ready.clone(), tail.clone());
        let deadline = Instant::now() + START_TIMEOUT;
        let mut announced = false;
        let result = loop {
            if !matches!(stop.try_recv(), Err(mpsc::TryRecvError::Empty)) {
                break Ok(());
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    let logs = tail
                        .lock()
                        .unwrap()
                        .iter()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join("\n");
                    break Err(format!("Whole-laptop routing exited ({status}). {logs}"));
                }
                Err(e) => break Err(format!("Cannot monitor whole-laptop routing: {e}")),
                Ok(None) => {}
            }
            if !announced && ready.load(Ordering::Acquire) {
                emit(Event::Log("[Whole laptop] Checking Windows route selection…".into()));
                match powershell("route verification", VERIFY_ROUTES) {
                    Ok(details) => emit(Event::Log(format!("[Whole laptop] {}", details.trim()))),
                    Err(error) => break Err(error),
                }
                if !matches!(stop.try_recv(), Err(mpsc::TryRecvError::Empty)) {
                    break Ok(());
                }
                emit(Event::Log("[Whole laptop] Checking TCP packet capture and return traffic…".into()));
                if let Some(check) = probe.take() {
                    if let Err(error) = check.verify() {
                        break Err(format!("Windows routes were installed, but TCP data did not return through the virtual adapter: {error}. Whole-laptop networking has been stopped."));
                    }
                }
                if !matches!(stop.try_recv(), Err(mpsc::TryRecvError::Empty)) {
                    break Ok(());
                }
                emit(Event::Log("[Whole laptop] TCP capture and return traffic passed. Checking DNS through Aether and the final proxy…".into()));
                if let Err(error) = check_dns("172.31.255.2:53".parse().unwrap()) {
                    break Err(format!("TCP capture passed, but the DNS round trip through Aether and the final proxy failed after capture started: {error}. Whole-laptop networking has been stopped."));
                }
                emit(Event::Log("[Whole laptop] DNS reply through Aether and the final proxy received. Whole-laptop checks passed.".into()));
                emit(Event::Ready);
                announced = true;
            }
            if !announced && Instant::now() >= deadline {
                break Err("Timed out creating the Windows virtual adapter.".into());
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        stop_child(&mut child);
        result
    })();
    let restored = cleanup(&request.data_dir);
    match (result, restored) {
        (Err(error), Err(recovery)) => Err(format!("{error}\n{recovery}")),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), result) => result,
    }
}

/// Pipe EOF is the GUI lifetime signal, including an ungraceful GUI crash. This
/// process has no listening socket and accepts no proxy credentials or shell
/// commands. It restores networking before exiting on EOF.
pub fn run_helper_if_requested() -> Option<i32> {
    if std::env::args_os().nth(1).as_deref() != Some(OsStr::new(HELPER_ARG)) {
        return None;
    }
    let mut input = BufReader::new(std::io::stdin());
    let mut line = String::new();
    let result = input
        .read_line(&mut line)
        .map_err(|e| e.to_string())
        .and_then(|_| serde_json::from_str::<Request>(&line).map_err(|e| e.to_string()));
    let result = result.and_then(|request| {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = input.read(&mut [0u8; 1]);
            let _ = tx.send(());
        });
        serve(request, rx)
    });
    match result {
        Ok(()) => Some(0),
        Err(error) => {
            emit(Event::Failed(error));
            Some(1)
        }
    }
}

pub struct Session {
    child: Child,
    input: Option<ChildStdin>,
    events: mpsc::Receiver<Event>,
    ready: bool,
    deadline: Instant,
    stopped: bool,
    logs: VecDeque<String>,
}

impl Session {
    pub fn start(request: Request) -> Result<Self, String> {
        preflight(&request)?;
        let mut child = Command::new(std::env::current_exe().map_err(|e| e.to_string())?)
            .arg(HELPER_ARG)
            // The helper allocates its own console after preserving the pipes.
            // DETACHED_PROCESS is the documented pairing with AllocConsole,
            // and also works when this executable uses the console subsystem.
            .creation_flags(DETACHED_PROCESS)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?;
        let input = child.stdin.take().ok_or("Missing helper control pipe")?;
        let output = child.stdout.take().ok_or("Missing helper status pipe")?;
        let (tx, events) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(output).lines().map_while(Result::ok) {
                if let Ok(event) = serde_json::from_str(&line) {
                    let _ = tx.send(event);
                }
            }
        });
        let mut session = Self {
            child,
            input: Some(input),
            events,
            ready: false,
            deadline: Instant::now() + Duration::from_secs(90),
            stopped: false,
            logs: VecDeque::new(),
        };
        let input = session.input.as_mut().unwrap();
        serde_json::to_writer(&mut *input, &request).map_err(|e| e.to_string())?;
        input.write_all(b"\n").map_err(|e| e.to_string())?;
        input.flush().map_err(|e| e.to_string())?;
        Ok(session)
    }

    /// True only after startup and Windows' actual route selection are checked.
    pub fn poll(&mut self) -> Result<bool, String> {
        for event in self.events.try_iter() {
            match event {
                Event::Ready => self.ready = true,
                Event::Failed(error) => return Err(error),
                Event::Log(line) => {
                    if self.logs.len() == 100 {
                        self.logs.pop_front();
                    }
                    self.logs.push_back(line);
                }
            }
        }
        if let Some(status) = self.child.try_wait().map_err(|e| e.to_string())? {
            // The reader may enqueue its last error concurrently with try_wait.
            let until = Instant::now() + Duration::from_millis(100);
            while let Ok(event) = self.events.recv_timeout(until.saturating_duration_since(Instant::now())) {
                match event {
                    Event::Failed(error) => return Err(error),
                    Event::Log(line) => {
                        if self.logs.len() == 100 {
                            self.logs.pop_front();
                        }
                        self.logs.push_back(line);
                    }
                    Event::Ready => self.ready = true,
                }
                if Instant::now() >= until {
                    break;
                }
            }
            return Err(format!(
                "Whole-laptop helper stopped ({status}). Use Restore normal networking if needed."
            ));
        }
        if !self.ready && Instant::now() >= self.deadline {
            return Err(
                "Timed out starting whole-laptop routing. Use Restore normal networking if needed."
                    .into(),
            );
        }
        Ok(self.ready)
    }

    pub fn take_logs(&mut self) -> Vec<String> {
        self.logs.drain(..).collect()
    }

    pub fn stop(&mut self) -> Result<(), String> {
        if self.stopped {
            return Ok(());
        }
        // EOF requests graceful cleanup, including routes. Startup can be in a
        // bounded PowerShell query when Cancel arrives.
        drop(self.input.take());
        let deadline = Instant::now() + Duration::from_secs(25);
        while Instant::now() < deadline {
            if let Ok(Some(status)) = self.child.try_wait() {
                self.stopped = true;
                if status.success() {
                    return Ok(());
                }
                while let Ok(event) = self.events.recv_timeout(Duration::from_millis(100)) {
                    if let Event::Failed(error) = event {
                        return Err(error);
                    }
                }
                return Err("Whole-laptop cleanup did not finish successfully. Use Restore normal networking.".into());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        // Job close is a last resort. A retained journal enables the explicit
        // recovery button on the next GUI launch if cleanup was interrupted.
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.stopped = true;
        Err(
            "Whole-laptop cleanup timed out. Use Restore normal networking, or restart Windows."
                .into(),
        )
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
    };

    #[test]
    fn absent_adapter_is_a_successful_empty_lookup() {
        // Exercise the real Windows PowerShell/NetAdapter combination. A
        // unique, absent name models first startup without touching adapters.
        let absent = format!("AetherMissing-{}", std::process::id());
        let query = ADAPTER_QUERY.replace(INTERFACE_NAME, &absent);
        let output = powershell(
            "adapter discovery",
            &format!("{query} | Select-Object -ExpandProperty Name"),
        )
        .expect("an adapter that has not been created is not a recovery error");
        assert!(output.trim().is_empty(), "{output}");
    }

    #[test]
    fn adapter_query_selects_only_the_owned_name() {
        // No real adapters are changed. Also cover a similarly named device
        // so discovery cannot authorize cleanup of another application's TUN.
        let fixture = r#"
function Get-NetAdapter {
    [CmdletBinding()]
    param([switch]$IncludeHidden)
    if (!$IncludeHidden) { throw 'Hidden adapters must be included' }
    @('Ethernet', 'AetherWholeLaptop-other', 'AetherWholeLaptop') | ForEach-Object {
        [pscustomobject]@{Name = $_}
    }
}
"#;
        let output = powershell(
            "adapter discovery",
            &format!("{fixture}\n{ADAPTER_QUERY} | Select-Object -ExpandProperty Name"),
        )
        .unwrap();
        assert_eq!(output.trim(), INTERFACE_NAME);
    }

    #[test]
    fn adapter_enumeration_errors_are_not_treated_as_absence() {
        let fixture = r#"
function Get-NetAdapter {
    [CmdletBinding()]
    param([switch]$IncludeHidden)
    Write-Error 'adapter enumeration denied'
}
"#;
        let error = powershell(
            "adapter discovery",
            &format!("{fixture}\n{ADAPTER_QUERY} | Select-Object -ExpandProperty Name"),
        )
        .unwrap_err();
        assert!(error.contains("adapter discovery"), "{error}");
        assert!(error.contains("adapter enumeration denied"), "{error}");
    }

    #[test]
    fn powershell_failure_without_output_still_explains_the_failure() {
        let error = powershell("adapter discovery", "exit 7").unwrap_err();
        assert!(error.contains("adapter discovery"), "{error}");
        assert!(error.contains("7"), "{error}");
        assert!(error.contains("no diagnostic output"), "{error}");
    }

    #[test]
    fn powershell_preserves_localized_error_text() {
        let error = powershell("adapter cleanup", "throw 'Réseau indisponible'").unwrap_err();
        assert!(error.contains("Réseau indisponible"), "{error}");
    }

    #[test]
    fn route_verification_requires_capture_routes_and_selected_interface() {
        // Script-level regression: validate the real readiness script without
        // creating routes. Find-NetRoute returns two different object types.
        let fixture = r#"
function Get-NetAdapter {
    [CmdletBinding()] param([switch]$IncludeHidden)
    [pscustomobject]@{Name = 'AetherWholeLaptop'; Status = 'Up'; ifIndex = 27}
}
function Get-NetRoute {
    [CmdletBinding()] param([int]$InterfaceIndex, [string]$PolicyStore)
    foreach ($prefix in @('0.0.0.0/1', '128.0.0.0/1', '::/1', '8000::/1')) {
        if ($missing -and $prefix -eq '128.0.0.0/1') { continue }
        [pscustomobject]@{DestinationPrefix = $prefix; InterfaceIndex = 27}
    }
}
function Find-NetRoute {
    [CmdletBinding()] param([string]$RemoteIPAddress)
    [pscustomobject]@{IPAddress = '172.31.255.1'; InterfaceIndex = 27}
    if ($wrong) {
        [pscustomobject]@{DestinationPrefix = '0.0.0.0/0'; InterfaceIndex = 9; InterfaceAlias = 'Other VPN'}
    } else {
        [pscustomobject]@{DestinationPrefix = '0.0.0.0/1'; InterfaceIndex = 27; InterfaceAlias = 'AetherWholeLaptop'}
    }
}
"#;
        let verify = |flags: &str| {
            powershell("route verification", &format!("{flags}\n{fixture}\n{VERIFY_ROUTES}"))
        };
        assert!(verify("$wrong = $false; $missing = $false").unwrap().contains("Windows selected AetherWholeLaptop"));
        let conflict = verify("$wrong = $true; $missing = $false").unwrap_err();
        assert!(conflict.contains("Windows selected 'Other VPN'"), "{conflict}");
        let missing = verify("$wrong = $false; $missing = $true").unwrap_err();
        assert!(missing.contains("missing its 128.0.0.0/1 capture route"), "{missing}");
    }

    struct ChildGuard(Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    // Isolated subprocess fixtures: neither test creates an adapter or changes
    // routing. The real hidden-console, pipe, CTRL_BREAK and job code is used.
    #[test]
    #[ignore = "invoked by supervision tests in an isolated subprocess"]
    fn sleeping_worker() {
        assert_eq!(std::env::var("AETHER_SUPERVISION_TEST").as_deref(), Ok("1"));
        loop {
            std::thread::sleep(Duration::from_secs(1));
        }
    }

    #[test]
    #[ignore = "invoked by supervision tests in an isolated subprocess"]
    fn supervision_worker() {
        assert_eq!(std::env::var("AETHER_SUPERVISION_TEST").as_deref(), Ok("1"));
        supervise_children().unwrap();
        hidden_console().unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "windows::tests::sleeping_worker",
                "--ignored",
                "--nocapture",
            ])
            .creation_flags(CREATE_NEW_PROCESS_GROUP)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        writeln!(std::io::stdout(), "SUPERVISED:{}", child.id()).unwrap();
        std::io::stdout().flush().unwrap();
        // Parent pipe EOF, the same signal used when the GUI closes/crashes.
        std::io::stdin().read_exact(&mut [0u8; 1]).unwrap_err();
        stop_child(&mut child);
        std::process::exit(0);
    }

    fn supervision_case(kill_helper: bool) {
        let mut helper = ChildGuard(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "windows::tests::supervision_worker",
                    "--ignored",
                    "--nocapture",
                ])
                .env("AETHER_SUPERVISION_TEST", "1")
                .creation_flags(DETACHED_PROCESS)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                // Surface an early child panic in CI instead of only reporting
                // that the readiness pipe disconnected.
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        );
        let reader = helper.0.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(reader).lines().map_while(Result::ok) {
                if let Some((_, pid)) = line.split_once("SUPERVISED:") {
                    let _ = tx.send(pid.parse::<u32>().unwrap());
                }
            }
        });
        let pid = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("helper must preserve its stdout pipe after AllocConsole");
        let process = unsafe { Handle(OpenProcess(PROCESS_SYNCHRONIZE, 0, pid)) };
        assert!(!process.0.is_null());
        if kill_helper {
            helper.0.kill().unwrap();
        }
        drop(helper.0.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(8);
        while helper.0.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "helper must stop on pipe EOF");
            std::thread::sleep(Duration::from_millis(50));
        }
        if !kill_helper {
            assert!(helper.0.wait().unwrap().success());
        }
        assert_eq!(
            unsafe { WaitForSingleObject(process.0, 3000) },
            0,
            "supervised descendant must exit even if the helper is killed"
        );
    }

    #[test]
    fn pipe_eof_closes_supervised_process() {
        supervision_case(false);
    }

    #[test]
    fn helper_crash_kills_descendants() {
        supervision_case(true);
    }
}
