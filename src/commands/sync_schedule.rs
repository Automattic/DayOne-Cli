use anyhow::{Context, Result, anyhow};
use serde::Serialize;
#[cfg(any(target_os = "macos", target_os = "linux", test))]
use std::fs;
#[cfg(any(target_os = "macos", target_os = "linux", test))]
use std::path::Path;
use std::path::PathBuf;
use std::process::{Command, ExitStatus, Stdio};

#[cfg(any(target_os = "macos", test))]
const JOB_ID: &str = "com.dayone.cli.sync";
#[cfg(any(target_os = "linux", test))]
const SYSTEMD_UNIT_NAME: &str = "dayone-cli-sync.service";
#[cfg(target_os = "linux")]
const SYSTEMD_TIMER_NAME: &str = "dayone-cli-sync.timer";
#[cfg(target_os = "windows")]
const WINDOWS_TASK_NAME: &str = r#"\DayOne CLI Sync"#;

#[derive(Debug, Clone)]
pub struct SyncScheduleArgs {
    pub config_dir: PathBuf,
    pub profile_name: String,
    pub base_url: String,
}

#[derive(Debug, Clone, Copy)]
pub struct EnableArgs {
    pub interval_minutes: u32,
}

#[derive(Debug, Serialize)]
pub struct SyncScheduleOutput {
    pub ok: bool,
    pub backend: String,
    pub registered: bool,
    pub enabled: bool,
    pub interval_minutes: Option<u32>,
    pub command: Vec<String>,
    pub path: Option<String>,
    pub details: Option<String>,
}

pub fn enable(args: SyncScheduleArgs, enable_args: EnableArgs) -> Result<SyncScheduleOutput> {
    let spec = ScheduleSpec::new(args, Some(enable_args.interval_minutes))?;
    spec.backend.enable(&spec)
}

pub fn status(args: SyncScheduleArgs) -> Result<SyncScheduleOutput> {
    let spec = ScheduleSpec::new(args, None)?;
    spec.backend.status(&spec)
}

pub fn disable(args: SyncScheduleArgs) -> Result<SyncScheduleOutput> {
    let spec = ScheduleSpec::new(args, None)?;
    spec.backend.disable(&spec)
}

struct ScheduleSpec {
    backend: Backend,
    config_dir: PathBuf,
    command: Vec<String>,
    interval_minutes: Option<u32>,
}

impl ScheduleSpec {
    fn new(args: SyncScheduleArgs, interval_minutes: Option<u32>) -> Result<Self> {
        let exe = std::env::current_exe().context("failed to resolve current executable path")?;
        let command = vec![
            exe.to_string_lossy().into_owned(),
            "--profile".to_owned(),
            args.profile_name,
            "--api-host".to_owned(),
            args.base_url,
            "sync".to_owned(),
        ];
        Ok(Self {
            backend: Backend::current(),
            config_dir: args.config_dir,
            command,
            interval_minutes,
        })
    }
}

#[derive(Debug, Clone, Copy)]
enum Backend {
    #[cfg(target_os = "macos")]
    MacLaunchd,
    #[cfg(target_os = "linux")]
    LinuxSystemdUser,
    #[cfg(target_os = "windows")]
    WindowsTaskScheduler,
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    Unsupported,
}

impl Backend {
    fn current() -> Self {
        #[cfg(target_os = "macos")]
        {
            Self::MacLaunchd
        }
        #[cfg(target_os = "linux")]
        {
            Self::LinuxSystemdUser
        }
        #[cfg(target_os = "windows")]
        {
            Self::WindowsTaskScheduler
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        {
            Self::Unsupported
        }
    }

    fn name(self) -> &'static str {
        match self {
            #[cfg(target_os = "macos")]
            Self::MacLaunchd => "launchd",
            #[cfg(target_os = "linux")]
            Self::LinuxSystemdUser => "systemd-user",
            #[cfg(target_os = "windows")]
            Self::WindowsTaskScheduler => "windows-task-scheduler",
            #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
            Self::Unsupported => "unsupported",
        }
    }

    fn enable(self, spec: &ScheduleSpec) -> Result<SyncScheduleOutput> {
        match self {
            #[cfg(target_os = "macos")]
            Self::MacLaunchd => macos_enable(spec),
            #[cfg(target_os = "linux")]
            Self::LinuxSystemdUser => linux_enable(spec),
            #[cfg(target_os = "windows")]
            Self::WindowsTaskScheduler => windows_enable(spec),
            #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
            Self::Unsupported => unsupported_output(spec, false),
        }
    }

    fn status(self, spec: &ScheduleSpec) -> Result<SyncScheduleOutput> {
        match self {
            #[cfg(target_os = "macos")]
            Self::MacLaunchd => macos_status(spec),
            #[cfg(target_os = "linux")]
            Self::LinuxSystemdUser => linux_status(spec),
            #[cfg(target_os = "windows")]
            Self::WindowsTaskScheduler => windows_status(spec),
            #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
            Self::Unsupported => unsupported_output(spec, false),
        }
    }

    fn disable(self, spec: &ScheduleSpec) -> Result<SyncScheduleOutput> {
        match self {
            #[cfg(target_os = "macos")]
            Self::MacLaunchd => macos_disable(spec),
            #[cfg(target_os = "linux")]
            Self::LinuxSystemdUser => linux_disable(spec),
            #[cfg(target_os = "windows")]
            Self::WindowsTaskScheduler => windows_disable(spec),
            #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
            Self::Unsupported => unsupported_output(spec, false),
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn unsupported_output(spec: &ScheduleSpec, ok: bool) -> Result<SyncScheduleOutput> {
    Ok(output(
        spec,
        ok,
        false,
        false,
        None,
        Some("scheduled sync is not supported on this operating system".to_owned()),
    ))
}

fn output(
    spec: &ScheduleSpec,
    ok: bool,
    registered: bool,
    enabled: bool,
    path: Option<PathBuf>,
    details: Option<String>,
) -> SyncScheduleOutput {
    SyncScheduleOutput {
        ok,
        backend: spec.backend.name().to_owned(),
        registered,
        enabled,
        interval_minutes: spec.interval_minutes,
        command: spec.command.clone(),
        path: path.map(|value| value.to_string_lossy().into_owned()),
        details,
    }
}

fn enable_interval(spec: &ScheduleSpec) -> u32 {
    spec.interval_minutes
        .expect("enable interval should be present")
}

fn quiet_status(command: &mut Command) -> std::io::Result<ExitStatus> {
    command.stdout(Stdio::null()).stderr(Stdio::null()).status()
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn quiet_success(command: &mut Command) -> bool {
    quiet_status(command)
        .map(|status| status.success())
        .unwrap_or(false)
}

fn ignore_quiet_status(command: &mut Command) {
    let _ = quiet_status(command);
}

#[cfg(target_os = "macos")]
fn macos_plist_path() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow!("failed to resolve home directory"))?;
    Ok(home
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{JOB_ID}.plist")))
}

#[cfg(target_os = "macos")]
fn macos_enable(spec: &ScheduleSpec) -> Result<SyncScheduleOutput> {
    let path = macos_plist_path()?;
    let interval = enable_interval(spec);
    let logs_dir = spec.config_dir.join("logs");
    fs::create_dir_all(&logs_dir).context("failed to create log directory")?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).context("failed to create LaunchAgents directory")?;
    }
    fs::write(
        &path,
        render_launchd_plist(
            interval,
            &spec.command,
            &logs_dir.join("sync-schedule.log"),
            &logs_dir.join("sync-schedule.err.log"),
        ),
    )
    .with_context(|| format!("failed to write {}", path.display()))?;

    let uid = crate::util::current_uid_string().context("failed to resolve current user ID")?;
    ignore_quiet_status(Command::new("launchctl").args([
        "bootout",
        &format!("gui/{uid}"),
        path.to_string_lossy().as_ref(),
    ]));
    let status = quiet_status(Command::new("launchctl").args([
        "bootstrap",
        &format!("gui/{uid}"),
        path.to_string_lossy().as_ref(),
    ]))
    .context("failed to run launchctl bootstrap")?;
    if !status.success() {
        return Err(anyhow!("launchctl bootstrap failed with status {status}"));
    }
    Ok(output(spec, true, true, true, Some(path), None))
}

#[cfg(target_os = "macos")]
fn macos_status(spec: &ScheduleSpec) -> Result<SyncScheduleOutput> {
    let path = macos_plist_path()?;
    let registered = path.exists();
    let uid = crate::util::current_uid_string().context("failed to resolve current user ID")?;
    let enabled =
        quiet_success(Command::new("launchctl").args(["print", &format!("gui/{uid}/{JOB_ID}")]));
    let mut result = output(spec, true, registered, enabled, Some(path.clone()), None);
    result.interval_minutes = read_launchd_interval_minutes(&path);
    Ok(result)
}

#[cfg(target_os = "macos")]
fn macos_disable(spec: &ScheduleSpec) -> Result<SyncScheduleOutput> {
    let path = macos_plist_path()?;
    let uid = crate::util::current_uid_string().context("failed to resolve current user ID")?;
    ignore_quiet_status(Command::new("launchctl").args([
        "bootout",
        &format!("gui/{uid}"),
        path.to_string_lossy().as_ref(),
    ]));
    if path.exists() {
        fs::remove_file(&path).with_context(|| format!("failed to remove {}", path.display()))?;
    }
    Ok(output(spec, true, false, false, Some(path), None))
}

#[cfg(any(target_os = "macos", test))]
fn render_launchd_plist(
    interval_minutes: u32,
    command: &[String],
    stdout_path: &Path,
    stderr_path: &Path,
) -> String {
    let args = command
        .iter()
        .map(|arg| format!("\t\t<string>{}</string>", xml_escape(arg)))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{}</string>
	<key>ProgramArguments</key>
	<array>
{}
	</array>
	<key>StartInterval</key>
	<integer>{}</integer>
	<key>RunAtLoad</key>
	<true/>
	<key>StandardOutPath</key>
	<string>{}</string>
	<key>StandardErrorPath</key>
	<string>{}</string>
</dict>
</plist>
"#,
        JOB_ID,
        args,
        interval_minutes * 60,
        xml_escape(&stdout_path.to_string_lossy()),
        xml_escape(&stderr_path.to_string_lossy())
    )
}

#[cfg(any(target_os = "macos", test))]
fn read_launchd_interval_minutes(path: &Path) -> Option<u32> {
    let contents = fs::read_to_string(path).ok()?;
    let marker = "<key>StartInterval</key>";
    let after_marker = contents.split(marker).nth(1)?;
    let start = after_marker.find("<integer>")? + "<integer>".len();
    let end = after_marker[start..].find("</integer>")? + start;
    after_marker[start..end]
        .trim()
        .parse::<u32>()
        .ok()
        .map(|seconds| seconds / 60)
}

#[cfg(target_os = "linux")]
fn systemd_user_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow!("failed to resolve home directory"))?;
    Ok(home.join(".config").join("systemd").join("user"))
}

#[cfg(target_os = "linux")]
fn linux_enable(spec: &ScheduleSpec) -> Result<SyncScheduleOutput> {
    let dir = systemd_user_dir()?;
    fs::create_dir_all(&dir).context("failed to create systemd user directory")?;
    let service_path = dir.join(SYSTEMD_UNIT_NAME);
    let timer_path = dir.join(SYSTEMD_TIMER_NAME);
    fs::write(&service_path, render_systemd_service(&spec.command))
        .with_context(|| format!("failed to write {}", service_path.display()))?;
    fs::write(&timer_path, render_systemd_timer(enable_interval(spec)))
        .with_context(|| format!("failed to write {}", timer_path.display()))?;
    run_systemctl(&["--user", "daemon-reload"])?;
    run_systemctl(&["--user", "enable", "--now", SYSTEMD_TIMER_NAME])?;
    Ok(output(spec, true, true, true, Some(timer_path), None))
}

#[cfg(target_os = "linux")]
fn linux_status(spec: &ScheduleSpec) -> Result<SyncScheduleOutput> {
    let timer_path = systemd_user_dir()?.join(SYSTEMD_TIMER_NAME);
    let registered = timer_path.exists();
    let enabled = quiet_success(Command::new("systemctl").args([
        "--user",
        "is-enabled",
        "--quiet",
        SYSTEMD_TIMER_NAME,
    ]));
    let mut result = output(
        spec,
        true,
        registered,
        enabled,
        Some(timer_path.clone()),
        None,
    );
    result.interval_minutes = read_systemd_interval_minutes(&timer_path);
    Ok(result)
}

#[cfg(target_os = "linux")]
fn linux_disable(spec: &ScheduleSpec) -> Result<SyncScheduleOutput> {
    let dir = systemd_user_dir()?;
    let service_path = dir.join(SYSTEMD_UNIT_NAME);
    let timer_path = dir.join(SYSTEMD_TIMER_NAME);
    ignore_quiet_status(Command::new("systemctl").args([
        "--user",
        "disable",
        "--now",
        SYSTEMD_TIMER_NAME,
    ]));
    for path in [&service_path, &timer_path] {
        if path.exists() {
            fs::remove_file(path)
                .with_context(|| format!("failed to remove {}", path.display()))?;
        }
    }
    ignore_quiet_status(Command::new("systemctl").args(["--user", "daemon-reload"]));
    Ok(output(spec, true, false, false, Some(timer_path), None))
}

#[cfg(any(target_os = "linux", test))]
fn render_systemd_service(command: &[String]) -> String {
    format!(
        "[Unit]\nDescription=Day One CLI sync\n\n[Service]\nType=oneshot\nExecStart={}\n",
        systemd_escape_command(command)
    )
}

#[cfg(any(target_os = "linux", test))]
fn render_systemd_timer(interval_minutes: u32) -> String {
    format!(
        "[Unit]\nDescription=Run Day One CLI sync every {interval_minutes} minutes\n\n[Timer]\nOnBootSec=1min\nOnUnitActiveSec={interval_minutes}min\nUnit={SYSTEMD_UNIT_NAME}\nPersistent=true\n\n[Install]\nWantedBy=timers.target\n"
    )
}

#[cfg(any(target_os = "linux", test))]
fn read_systemd_interval_minutes(path: &Path) -> Option<u32> {
    let contents = fs::read_to_string(path).ok()?;
    contents.lines().find_map(|line| {
        let value = line.strip_prefix("OnUnitActiveSec=")?;
        value.strip_suffix("min")?.parse::<u32>().ok()
    })
}

#[cfg(target_os = "linux")]
fn run_systemctl(args: &[&str]) -> Result<()> {
    let status = quiet_status(Command::new("systemctl").args(args))
        .with_context(|| format!("failed to run systemctl {}", args.join(" ")))?;
    if !status.success() {
        return Err(anyhow!(
            "systemctl {} failed with status {status}",
            args.join(" ")
        ));
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn windows_enable(spec: &ScheduleSpec) -> Result<SyncScheduleOutput> {
    let interval = enable_interval(spec);
    let command = &spec.command[0];
    let args = spec.command[1..].join(" ");
    let status = quiet_status(Command::new("schtasks.exe").args([
        "/Create",
        "/F",
        "/TN",
        WINDOWS_TASK_NAME,
        "/SC",
        "MINUTE",
        "/MO",
        &interval.to_string(),
        "/TR",
        &format!("\"{command}\" {args}"),
    ]))
    .context("failed to run schtasks.exe /Create")?;
    if !status.success() {
        return Err(anyhow!("schtasks.exe /Create failed with status {status}"));
    }
    Ok(output(spec, true, true, true, None, None))
}

#[cfg(target_os = "windows")]
fn windows_status(spec: &ScheduleSpec) -> Result<SyncScheduleOutput> {
    let query = Command::new("schtasks.exe")
        .args(["/Query", "/TN", WINDOWS_TASK_NAME, "/XML"])
        .output();
    let Ok(query) = query else {
        return Ok(output(spec, true, false, false, None, None));
    };
    let registered = query.status.success();
    let xml = String::from_utf8_lossy(&query.stdout);
    let enabled = registered && xml.contains("<Enabled>true</Enabled>");
    let mut result = output(spec, true, registered, enabled, None, None);
    result.interval_minutes = read_windows_interval_minutes(&xml);
    Ok(result)
}

#[cfg(target_os = "windows")]
fn windows_disable(spec: &ScheduleSpec) -> Result<SyncScheduleOutput> {
    ignore_quiet_status(Command::new("schtasks.exe").args([
        "/Delete",
        "/F",
        "/TN",
        WINDOWS_TASK_NAME,
    ]));
    Ok(output(spec, true, false, false, None, None))
}

#[cfg(any(target_os = "windows", test))]
fn read_windows_interval_minutes(xml: &str) -> Option<u32> {
    let start = xml.find("<Interval>PT")? + "<Interval>PT".len();
    let end = xml[start..].find("M</Interval>")? + start;
    xml[start..end].parse().ok()
}

#[cfg(any(target_os = "macos", test))]
fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(any(target_os = "linux", test))]
fn systemd_escape_command(command: &[String]) -> String {
    command
        .iter()
        .map(|part| {
            if part.chars().all(|ch| {
                ch.is_ascii_alphanumeric() || matches!(ch, '/' | '.' | '_' | '-' | ':' | '=')
            }) {
                part.clone()
            } else {
                format!("'{}'", part.replace('\\', "\\\\").replace('\'', "'\\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_native_scheduler_backend() {
        let expected = match std::env::consts::OS {
            "macos" => "launchd",
            "linux" => "systemd-user",
            "windows" => "windows-task-scheduler",
            _ => "unsupported",
        };
        assert_eq!(Backend::current().name(), expected);
    }

    #[test]
    fn schedule_spec_preserves_profile_and_api_host() {
        let spec = ScheduleSpec::new(
            SyncScheduleArgs {
                config_dir: PathBuf::from("config directory"),
                profile_name: "custom profile".to_owned(),
                base_url: "https://example.invalid".to_owned(),
            },
            Some(30),
        )
        .expect("schedule specification");

        assert_eq!(spec.backend.name(), Backend::current().name());
        assert_eq!(spec.config_dir, PathBuf::from("config directory"));
        assert_eq!(spec.interval_minutes, Some(30));
        assert_eq!(
            spec.command[0],
            std::env::current_exe()
                .expect("test executable")
                .to_string_lossy()
        );
        assert_eq!(
            &spec.command[1..],
            [
                "--profile",
                "custom profile",
                "--api-host",
                "https://example.invalid",
                "sync"
            ]
        );
    }

    #[test]
    fn launchd_plist_contains_interval_and_arguments() {
        let plist = render_launchd_plist(
            30,
            &["/bin/dayone".to_owned(), "sync".to_owned()],
            Path::new("/tmp/out.log"),
            Path::new("/tmp/err.log"),
        );
        assert!(plist.contains("<integer>1800</integer>"));
        assert!(plist.contains("<string>/bin/dayone</string>"));
        assert!(plist.contains("<string>sync</string>"));
    }

    #[test]
    fn reads_launchd_interval_from_plist() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sync.plist");
        let plist = render_launchd_plist(
            45,
            &["dayone".to_owned(), "sync".to_owned()],
            Path::new("out.log"),
            Path::new("err.log"),
        );
        fs::write(&path, plist).expect("write plist");
        assert_eq!(read_launchd_interval_minutes(&path), Some(45));
    }

    #[test]
    fn systemd_service_contains_sync_command() {
        let service = render_systemd_service(&["dayone".to_owned(), "sync".to_owned()]);
        assert!(service.contains("ExecStart=dayone sync\n"));
    }

    #[test]
    fn systemd_timer_contains_interval() {
        let timer = render_systemd_timer(15);
        assert!(timer.contains("OnUnitActiveSec=15min"));
        assert!(timer.contains(SYSTEMD_UNIT_NAME));
    }

    #[test]
    fn reads_systemd_interval_from_timer() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("timer");
        fs::write(&path, render_systemd_timer(20)).expect("write timer");
        assert_eq!(read_systemd_interval_minutes(&path), Some(20));
    }

    #[test]
    fn reads_windows_interval_from_task_xml() {
        let xml = "<Task><Triggers><CalendarTrigger><Repetition><Interval>PT45M</Interval></Repetition></CalendarTrigger></Triggers></Task>";
        assert_eq!(read_windows_interval_minutes(xml), Some(45));
    }

    #[test]
    fn systemd_command_quotes_spaces() {
        let command = systemd_escape_command(&[
            "/Applications/Day One/dayone".to_owned(),
            "--profile".to_owned(),
            "default".to_owned(),
        ]);
        assert!(command.starts_with("'/Applications/Day One/dayone' --profile default"));
    }
}
