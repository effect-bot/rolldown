//! Limits for the fixtures that run `experimental.effectServiceShaking`.
//!
//! Each fixture is a small program, so its analysis gets a small entries
//! budget, [`MAX_ENTRIES`], unless its config sets
//! `effectServiceShakingMaxEntries` itself (the dedicated budget fixture,
//! `tree_shaking/effect_service/budget_exceeded_keeps_every_method`, does).
//! The largest fixture solve takes about 17k entries, so a fixture that needs
//! more than the budget has regressed. The analysis's own default, 32M, is for
//! real programs.
//!
//! Each fixture must also finish within a time limit, measured over the whole
//! fixture: every build and the run of its `_test.mjs`. The limit is
//! `ROLLDOWN_TEST_EFFECT_FIXTURE_TIME_LIMIT_MS` in milliseconds,
//! [`DEFAULT_TIME_LIMIT_MS`] when unset. `0` disables it. The slowest fixture
//! takes about 0.7 s in a parallel run of the suite on a 16-core host.
//!
//! The limit is a deadline, not a measurement taken afterwards. A hung
//! analysis blocks its thread and a hung `_test.mjs` blocks the wait for
//! Node, so nothing inside the test's own process can stop them. Instead the
//! test runs the fixture again in a child process, the same test binary
//! filtered to the same test ([`run_in_child`]), and owns every process the
//! child starts:
//!
//! - On Linux the child runs in a PID namespace of its own, as its first
//!   process (PID 1), under a small supervisor process ([`namespace`]). When
//!   PID 1 exits or is killed, the kernel kills every other process in the
//!   namespace, and a process cannot leave it: `setsid` (Node's
//!   `detached: true`) does not. The supervisor exits only after that, so its
//!   exit confirms the namespace is empty. Creating the namespace needs
//!   unprivileged user namespaces; [`containment`] says whether this host has
//!   them.
//! - On other Unix systems, and on Linux without user namespaces, the child
//!   leads a new process group, which Node and anything it starts join. The
//!   group is signalled while the child is still unreaped, so its id cannot
//!   have been reused. A process that leaves the group (`setsid`) is not
//!   stopped, and the harness says so instead of reporting every process
//!   stopped.
//! - On Windows the child is put in a job object before it starts anything
//!   (it waits for its stdin to close, [`wait_until_owned`]). Processes it
//!   starts belong to the job too, and closing the job kills whatever is left.
//!
//! The parent stops the tree when the child exits, at the deadline, and when
//! the parent unwinds from a panic. A failure to stop it fails the test and
//! says so. The parent's own destruction (`SIGKILL`, or an abort that does not
//! unwind) runs none of that. On Linux with the namespace the supervisor and
//! PID 1 die with the parent's thread (`PR_SET_PDEATHSIG`), which ends the
//! namespace; with a process group nothing stops it. Windows's job is closed,
//! and its processes killed, when the parent exits for any reason.

use std::{
  fs::File,
  io::{self, Read},
  process::{Child, Command, ExitStatus, Stdio},
  sync::atomic::{AtomicU64, Ordering},
  time::{Duration, Instant},
};

/// Entries budget (points-to and copy entries) for a fixture's analysis.
pub const MAX_ENTRIES: u64 = 100_000;

/// Default time limit for one fixture, in milliseconds.
pub const DEFAULT_TIME_LIMIT_MS: u64 = 2_000;

const TIME_LIMIT_ENV: &str = "ROLLDOWN_TEST_EFFECT_FIXTURE_TIME_LIMIT_MS";

/// Set in the child process that runs one fixture for its parent.
const CHILD_ENV: &str = "ROLLDOWN_TEST_EFFECT_FIXTURE_CHILD";

/// The time limit from the environment, `None` when disabled.
pub fn time_limit() -> Option<Duration> {
  let ms = match std::env::var(TIME_LIMIT_ENV) {
    Ok(value) => value.trim().parse().unwrap_or_else(|_| {
      panic!("{TIME_LIMIT_ENV} should be a whole number of milliseconds, got {value:?}")
    }),
    Err(_) => DEFAULT_TIME_LIMIT_MS,
  };
  (ms > 0).then(|| Duration::from_millis(ms))
}

/// Whether this process is the child that runs one fixture ([`run_in_child`]).
pub fn is_child() -> bool {
  std::env::var_os(CHILD_ENV).is_some()
}

/// `Ok` when every process a fixture starts stays in its process tree, so stopping the tree stops
/// all of them; otherwise why not.
pub fn containment() -> Result<(), String> {
  tree::containment()
}

/// Called by the child before it starts anything: returns once the parent has closed the
/// child's stdin, which it does once the child belongs to the parent's process tree. On Windows
/// a process the child started before then would not be in the tree.
pub fn wait_until_owned() {
  let _ = io::stdin().read_to_end(&mut Vec::new());
}

/// Runs the fixture `name` by running the libtest test `test` of this binary in a
/// child process, and panics with the child's output if the test fails, is still running at
/// `limit`, or leaves processes that cannot be stopped.
pub fn run_in_child(name: &str, test: &str, limit: Duration) {
  static NEXT: AtomicU64 = AtomicU64::new(0);
  let log_path = std::env::temp_dir().join(format!(
    "rolldown-effect-fixture-{}-{}.log",
    std::process::id(),
    NEXT.fetch_add(1, Ordering::Relaxed)
  ));
  let log = File::create(&log_path).expect("failed to create the fixture's log file");
  let mut command = Command::new(std::env::current_exe().expect("no path to the test binary"));
  command
    .args([test, "--exact", "--test-threads=1"])
    .env(CHILD_ENV, "1")
    .stdout(log.try_clone().expect("failed to share the fixture's log file"))
    .stderr(log);
  let started = Instant::now();
  let finished = run_with_deadline(command, limit);
  let output = std::fs::read_to_string(&log_path).unwrap_or_default();
  let _ = std::fs::remove_file(&log_path);
  if let Err(message) = verdict(name, limit, started.elapsed(), &finished, &output) {
    panic!("{message}");
  }
}

/// How a process run by [`run_with_deadline`] ended.
#[derive(Debug)]
pub struct Finished {
  /// Its exit status, or `None` when the deadline came first.
  pub status: Option<ExitStatus>,
  /// Whether its process tree was stopped: the processes it left when it exited, or all of
  /// them at the deadline.
  pub stopped: io::Result<()>,
  /// [`containment`] for the tree that was stopped.
  pub containment: Result<(), String>,
}

/// The test's result, from how its child process ended and what it printed.
fn verdict(
  name: &str,
  limit: Duration,
  elapsed: Duration,
  finished: &Finished,
  output: &str,
) -> Result<(), String> {
  let not_stopped = |err: &io::Error| {
    format!(
      "the harness could not stop the processes it started ({err}), so they may still be running"
    )
  };
  match (finished.status, &finished.stopped) {
    (Some(status), Ok(())) if status.success() && output.contains(" 1 passed;") => Ok(()),
    (Some(status), Ok(())) => Err(format!("{name} failed in its child process ({status}):\n{output}")),
    (Some(status), Err(err)) => Err(format!(
      "{name} exited from its child process ({status}), but {}. Its output:\n{output}",
      not_stopped(err)
    )),
    (None, stopped) => Err(format!(
      "{name} did not finish within the {:.2} s time limit for an Effect service fixture \
       ({TIME_LIMIT_ENV}); after {:.2} s {}. Its output:\n{output}",
      limit.as_secs_f64(),
      elapsed.as_secs_f64(),
      match (stopped, &finished.containment) {
        (Err(err), _) => not_stopped(err),
        (Ok(()), Ok(())) => "stopped it and every process it started".into(),
        (Ok(()), Err(reason)) => format!(
          "stopped its process group, but not any process that left the group (`setsid`, Node's \
           `detached: true`), which may still be running: {reason}"
        ),
      },
    )),
  }
}

/// Runs `command` in a process tree of its own and waits for it until `limit` has passed.
/// Its stdin is a pipe that is closed once the process belongs to the tree. When it exits, the
/// processes it left are stopped; at the deadline, all of them are.
pub fn run_with_deadline(mut command: Command, limit: Duration) -> Finished {
  tree::prepare(&mut command);
  command.stdin(Stdio::piped());
  let containment = tree::containment();
  let mut owned = Owned::adopt(command.spawn().expect("failed to start the fixture's process"));
  let deadline = Instant::now() + limit;
  loop {
    if tree::exited(&mut owned.child).expect("failed to wait for the fixture's process") {
      let (stopped, status) = owned.stop();
      let status = status.expect("failed to reap the fixture's process");
      return Finished { status: Some(status), stopped, containment };
    }
    if Instant::now() >= deadline {
      let (stopped, status) = owned.stop();
      return Finished { status: None, stopped: stopped.and(status.map(drop)), containment };
    }
    std::thread::sleep(Duration::from_millis(5));
  }
}

/// A child process and the tree of processes it starts, stopped on drop if not before.
struct Owned {
  child: Child,
  tree: tree::Tree,
  stopped: bool,
}

impl Owned {
  fn adopt(mut child: Child) -> Self {
    match tree::Tree::adopt(&child) {
      Ok(tree) => {
        // The child starts nothing before this ([`wait_until_owned`]).
        drop(child.stdin.take());
        Self { child, tree, stopped: false }
      }
      Err(err) => {
        let _ = child.kill();
        let _ = child.wait();
        panic!("failed to give the fixture's process a process tree of its own: {err}");
      }
    }
  }

  /// Kills every process left in the tree, then reaps the child.
  fn stop(&mut self) -> (io::Result<()>, io::Result<ExitStatus>) {
    self.stopped = true;
    let killed = self.tree.kill();
    if killed.is_err() {
      // Without this, waiting for a child that is still running would never return.
      let _ = self.child.kill();
    }
    (killed, self.child.wait())
  }
}

impl Drop for Owned {
  #[expect(clippy::print_stderr, reason = "a drop cannot fail the test, so it reports")]
  fn drop(&mut self) {
    if !self.stopped {
      let (killed, status) = self.stop();
      if let Err(err) = killed.and(status.map(drop)) {
        eprintln!("failed to stop an Effect service fixture's processes: {err}");
      }
    }
  }
}

#[cfg(unix)]
mod tree {
  use std::{
    io,
    os::unix::process::CommandExt,
    process::{Child, Command},
    time::{Duration, Instant},
  };

  /// The child's process group, and whether the child runs in a PID namespace ([`super::namespace`]).
  pub struct Tree {
    group: libc::pid_t,
    contained: bool,
  }

  pub fn containment() -> Result<(), String> {
    #[cfg(target_os = "linux")]
    return super::namespace::available();
    #[cfg(not(target_os = "linux"))]
    Err("this platform has no PID namespaces".into())
  }

  /// Makes the child the leader of a new process group, whose id is the child's pid, and on
  /// Linux the supervisor of a PID namespace.
  pub fn prepare(command: &mut Command) {
    command.process_group(0);
    #[cfg(target_os = "linux")]
    if containment().is_ok() {
      super::namespace::supervise(command);
    }
  }

  impl Tree {
    #[expect(clippy::unnecessary_wraps, reason = "the group is made at spawn; Windows can fail here")]
    pub fn adopt(child: &Child) -> io::Result<Self> {
      Ok(Self { group: pid(child), contained: containment().is_ok() })
    }

    /// Kills every process in the tree.
    ///
    /// In a PID namespace the supervisor kills PID 1 on `SIGTERM` and exits once the kernel has
    /// killed everything else in the namespace, so its exit is the confirmation. Otherwise every
    /// process in the group is killed; the leader is still unreaped, so the group still exists
    /// and its id is still its own.
    pub fn kill(&self) -> io::Result<()> {
      if self.contained {
        signal(self.group, libc::SIGTERM)?;
        let started = Instant::now();
        while !exited_pid(self.group)? {
          if started.elapsed() > Duration::from_secs(5) {
            return Err(io::Error::new(
              io::ErrorKind::TimedOut,
              "the fixture's PID namespace still had processes 5 s after it was stopped",
            ));
          }
          std::thread::sleep(Duration::from_millis(5));
        }
        return Ok(());
      }
      signal(-self.group, libc::SIGKILL)
    }
  }

  fn signal(pid: libc::pid_t, signal: libc::c_int) -> io::Result<()> {
    // SAFETY: plain `kill(2)`; a negative pid signals the process group.
    if unsafe { libc::kill(pid, signal) } == 0 {
      return Ok(());
    }
    let err = io::Error::last_os_error();
    // Nothing is left to signal (macOS does not count a zombie leader).
    if err.raw_os_error() == Some(libc::ESRCH) { Ok(()) } else { Err(err) }
  }

  /// Whether the child has exited, leaving it unreaped (`WNOWAIT`): its pid, which is also
  /// the group's id, cannot be reused before the group is killed.
  #[expect(clippy::needless_pass_by_ref_mut, reason = "Windows's `try_wait` needs it")]
  pub fn exited(child: &mut Child) -> io::Result<bool> {
    exited_pid(pid(child))
  }

  fn exited_pid(pid: libc::pid_t) -> io::Result<bool> {
    // SAFETY: an all-zero `siginfo_t` is valid, and `waitid` fills it in.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    #[expect(clippy::cast_sign_loss)]
    let id = pid as libc::id_t;
    // SAFETY: `info` outlives the call.
    let result = unsafe {
      libc::waitid(libc::P_PID, id, &raw mut info, libc::WEXITED | libc::WNOHANG | libc::WNOWAIT)
    };
    if result != 0 {
      let err = io::Error::last_os_error();
      return if err.kind() == io::ErrorKind::Interrupted { Ok(false) } else { Err(err) };
    }
    // `si_pid` stays 0 while the child is running.
    // SAFETY: `waitid` filled in `info`.
    Ok(unsafe { info.si_pid() } != 0)
  }

  fn pid(child: &Child) -> libc::pid_t {
    child.id() as libc::pid_t
  }
}

/// The Linux PID namespace a fixture runs in.
///
/// The process the harness spawns becomes a supervisor: before `exec` it creates a user and PID
/// namespace, forks the namespace's first process (PID 1), which goes on to exec the test binary,
/// and waits for it. The kernel kills every other process in a namespace once its PID 1 has
/// exited, before the exit is reported, so the supervisor's own exit means the namespace is
/// empty. `SIGTERM` to the supervisor kills PID 1. The user namespace maps only the caller's own
/// uid and gid, so files and permissions look the same inside it.
#[cfg(target_os = "linux")]
mod namespace {
  use std::{
    ffi::CStr,
    io,
    os::unix::process::CommandExt,
    process::Command,
    sync::{
      OnceLock,
      atomic::{AtomicI32, Ordering},
    },
  };

  const FLAGS: libc::c_int = libc::CLONE_NEWUSER | libc::CLONE_NEWPID;

  /// Whether this host lets an unprivileged process create the namespaces, checked once.
  pub fn available() -> Result<(), String> {
    static PROBE: OnceLock<Result<(), String>> = OnceLock::new();
    PROBE.get_or_init(probe).clone()
  }

  fn probe() -> Result<(), String> {
    // SAFETY: the forked process only makes system calls before `_exit`.
    let pid = unsafe { libc::fork() };
    if pid == -1 {
      return Err(format!("fork failed: {}", io::Error::last_os_error()));
    }
    if pid == 0 {
      // SAFETY: as above.
      unsafe {
        let code = if libc::unshare(FLAGS) == 0 { 0 } else { *libc::__errno_location() };
        libc::_exit(code & 0xff);
      }
    }
    let mut status = 0;
    // SAFETY: `status` outlives the call.
    while unsafe { libc::waitpid(pid, &raw mut status, 0) } == -1 {
      let err = io::Error::last_os_error();
      if err.kind() != io::ErrorKind::Interrupted {
        return Err(format!("waitpid failed: {err}"));
      }
    }
    match (libc::WIFEXITED(status), libc::WEXITSTATUS(status)) {
      (true, 0) => Ok(()),
      (true, code) => Err(format!(
        "unprivileged user and PID namespaces are not available ({}); they need \
         user.max_user_namespaces > 0 and, on Ubuntu 23.10 and later, \
         kernel.apparmor_restrict_unprivileged_userns = 0",
        io::Error::from_raw_os_error(code)
      )),
      _ => Err(format!("the namespace probe ended abnormally ({status})")),
    }
  }

  /// Makes the spawned process the supervisor of a new PID namespace.
  pub fn supervise(command: &mut Command) {
    let watchdog = libc::pid_t::try_from(std::process::id()).expect("pid fits pid_t");
    // SAFETY: neither call can fail.
    let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
    let uid_map = format!("{uid} {uid} 1").into_bytes();
    let gid_map = format!("{gid} {gid} 1").into_bytes();
    // SAFETY: `supervisor` makes only async-signal-safe calls, as the forked child of a
    // multithreaded process must.
    unsafe { command.pre_exec(move || supervisor(watchdog, &uid_map, &gid_map)) };
  }

  /// PID 1, as the supervisor sees it.
  static INIT: AtomicI32 = AtomicI32::new(0);

  extern "C" fn kill_init(_: libc::c_int) {
    let init = INIT.load(Ordering::Relaxed);
    if init > 0 {
      // SAFETY: `kill` is async-signal-safe.
      unsafe { libc::kill(init, libc::SIGKILL) };
    }
  }

  fn check(result: libc::c_int) -> io::Result<()> {
    if result == -1 { Err(io::Error::last_os_error()) } else { Ok(()) }
  }

  /// Writes `content` to the file at `path` in one `write`, as the id map files require.
  fn write(path: &CStr, content: &[u8]) -> io::Result<()> {
    // SAFETY: `path` is NUL-terminated and `content` outlives the write.
    unsafe {
      let fd = libc::open(path.as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC);
      check(fd)?;
      let written = libc::write(fd, content.as_ptr().cast(), content.len());
      let err = io::Error::last_os_error();
      libc::close(fd);
      if written == -1 { Err(err) } else { Ok(()) }
    }
  }

  /// Runs in the spawned process between `fork` and `exec`. Returns only in PID 1, which then
  /// execs the test binary; the supervisor waits for PID 1 and exits with its status.
  fn supervisor(watchdog: libc::pid_t, uid_map: &[u8], gid_map: &[u8]) -> io::Result<()> {
    // SAFETY: async-signal-safe calls only, on memory this process owns.
    unsafe {
      // The supervisor dies with the harness thread that spawned it.
      check(libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL))?;
      if libc::getppid() != watchdog {
        libc::_exit(1);
      }
      check(libc::unshare(FLAGS))?;
      write(c"/proc/self/uid_map", uid_map)?;
      write(c"/proc/self/setgroups", b"deny")?;
      write(c"/proc/self/gid_map", gid_map)?;
      // A `SIGTERM` that comes before the handler is installed waits for it.
      let mut term: libc::sigset_t = std::mem::zeroed();
      let mut old: libc::sigset_t = std::mem::zeroed();
      libc::sigemptyset(&raw mut term);
      libc::sigaddset(&raw mut term, libc::SIGTERM);
      libc::pthread_sigmask(libc::SIG_BLOCK, &raw const term, &raw mut old);
      let init = libc::fork();
      if init == -1 {
        return Err(io::Error::last_os_error());
      }
      if init == 0 {
        // PID 1. It dies with the supervisor, which ends the namespace.
        check(libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL))?;
        libc::pthread_sigmask(libc::SIG_SETMASK, &raw const old, std::ptr::null_mut());
        return Ok(());
      }
      INIT.store(init, Ordering::Relaxed);
      let mut action: libc::sigaction = std::mem::zeroed();
      #[expect(clippy::fn_to_numeric_cast_any, reason = "`sigaction` takes the handler as an address")]
      let handler = kill_init as extern "C" fn(libc::c_int) as libc::sighandler_t;
      action.sa_sigaction = handler;
      libc::sigaction(libc::SIGTERM, &raw const action, std::ptr::null_mut());
      // The supervisor never execs, so it closes what `exec` would: the harness waits for the
      // pipe that tells it the exec succeeded to close, and only PID 1's copy closes at exec.
      if libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 0u32) != 0 {
        for fd in 3..4096 {
          libc::close(fd);
        }
      }
      libc::pthread_sigmask(libc::SIG_SETMASK, &raw const old, std::ptr::null_mut());
      let mut status = 0;
      while libc::waitpid(init, &raw mut status, 0) != init {
        if *libc::__errno_location() != libc::EINTR {
          libc::_exit(127);
        }
      }
      if libc::WIFEXITED(status) {
        libc::_exit(libc::WEXITSTATUS(status));
      }
      let signal = libc::WTERMSIG(status);
      libc::signal(signal, libc::SIG_DFL);
      libc::raise(signal);
      libc::_exit(128 + signal)
    }
  }
}

#[cfg(windows)]
mod tree {
  use std::{
    io,
    os::windows::io::AsRawHandle,
    process::{Child, Command},
  };

  use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE},
    System::JobObjects::{
      AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
      JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
      SetInformationJobObject, TerminateJobObject,
    },
  };

  /// A job object holding the child. Processes it starts join the job, and closing the job's
  /// last handle, on drop or when this process ends, kills every process still in it.
  pub struct Tree {
    job: HANDLE,
  }

  pub fn prepare(_command: &mut Command) {}

  /// A process cannot leave the job: the job does not allow breakaway.
  #[expect(clippy::unnecessary_wraps, reason = "Unix hosts may lack containment")]
  pub fn containment() -> Result<(), String> {
    Ok(())
  }

  fn check(ok: i32) -> io::Result<()> {
    if ok == 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
  }

  impl Tree {
    pub fn adopt(child: &Child) -> io::Result<Self> {
      // SAFETY: no security attributes and no name.
      let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
      if job.is_null() {
        return Err(io::Error::last_os_error());
      }
      let tree = Self { job };
      // SAFETY: an all-zero limit structure is valid.
      let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
      limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
      // SAFETY: `limits` is the structure this information class takes, and outlives the call.
      check(unsafe {
        SetInformationJobObject(
          tree.job,
          JobObjectExtendedLimitInformation,
          (&raw const limits).cast(),
          u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>()).unwrap(),
        )
      })?;
      // SAFETY: the child's handle is open while `child` is borrowed.
      check(unsafe { AssignProcessToJobObject(tree.job, child.as_raw_handle()) })?;
      Ok(tree)
    }

    /// Kills every process in the job.
    pub fn kill(&self) -> io::Result<()> {
      // SAFETY: the job handle is open until drop.
      check(unsafe { TerminateJobObject(self.job, 1) })
    }
  }

  impl Drop for Tree {
    fn drop(&mut self) {
      // SAFETY: the job handle is open and closed only here.
      unsafe { CloseHandle(self.job) };
    }
  }

  /// Whether the child has exited. A Windows process handle keeps its id from being reused.
  pub fn exited(child: &mut Child) -> io::Result<bool> {
    Ok(child.try_wait()?.is_some())
  }
}

#[cfg(test)]
mod tests {
  #[cfg(unix)]
  use std::os::unix::process::ExitStatusExt as _;
  #[cfg(windows)]
  use std::os::windows::process::ExitStatusExt as _;
  use std::{io, process::ExitStatus, time::Duration};

  use super::{Finished, verdict};

  const PASSED: &str = "test result: ok. 1 passed; 0 failed";

  fn verdict_of(
    status: Option<ExitStatus>,
    stopped: io::Result<()>,
    containment: Result<(), String>,
    output: &str,
  ) -> Result<(), String> {
    let finished = Finished { status, stopped, containment };
    verdict("fixture", Duration::from_secs(2), Duration::from_secs(2), &finished, output)
  }

  #[test]
  fn a_tree_that_cannot_be_stopped_is_reported() {
    let denied = || Err(io::Error::from(io::ErrorKind::PermissionDenied));
    let timed_out = verdict_of(None, denied(), Ok(()), "").unwrap_err();
    assert!(timed_out.contains("could not stop"), "{timed_out}");
    assert!(!timed_out.contains("stopped it"), "{timed_out}");
    let passed = verdict_of(Some(ExitStatus::from_raw(0)), denied(), Ok(()), PASSED).unwrap_err();
    assert!(passed.contains("could not stop"), "{passed}");
  }

  #[test]
  fn a_stopped_tree_is_reported() {
    let timed_out = verdict_of(None, Ok(()), Ok(()), "").unwrap_err();
    assert!(timed_out.contains("stopped it and every process it started"), "{timed_out}");
    assert_eq!(verdict_of(Some(ExitStatus::from_raw(0)), Ok(()), Ok(()), PASSED), Ok(()));
  }

  /// Without containment, a stopped process group is not reported as every process stopped.
  #[test]
  fn a_stopped_group_is_not_reported_as_every_process() {
    let timed_out = verdict_of(None, Ok(()), Err("no namespaces".into()), "").unwrap_err();
    assert!(!timed_out.contains("every process"), "{timed_out}");
    assert!(timed_out.contains("may still be running: no namespaces"), "{timed_out}");
  }
}
