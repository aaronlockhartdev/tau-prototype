use super::args::{BashArgs, EditArgs, ReadArgs, WriteArgs};
use super::impls::{
    bash_inner as bash_inner_impl, edit as edit_impl, read as read_impl, write as write_impl,
};
use super::*;
pub use impls::bash;
use std::sync::atomic::AtomicBool;

// The core tools as the test files call them: the model's raw JSON in, the
// parsed args out — the same parse `dispatch` runs, its refusal of a missing
// required argument included (audit Add5-T1). An argument a test names that
// the schema does not declare is refused at the seam the live dispatch
// refuses it, so a shape test exercises the shipped contract, not a copy.

pub fn read(
    cwd: &Path,
    args: &serde_json::Value,
    image_max_bytes: Option<u64>,
    image_max_pixels: Option<u64>,
    registry: Option<&mut hashline::registry::Registry>,
) -> ToolResult {
    match ReadArgs::parse(args) {
        Ok(a) => read_impl(cwd, &a, image_max_bytes, image_max_pixels, registry),
        Err(e) => ToolResult::err(e.into()),
    }
}

pub async fn write(
    cwd: &Path,
    args: &serde_json::Value,
    progress: Option<&crate::tools::ToolProgress>,
) -> ToolResult {
    match WriteArgs::parse(args) {
        Ok(a) => write_impl(cwd, &a, progress).await,
        Err(e) => ToolResult::err(e.into()),
    }
}

/// `strict` arrives as the parameter the tests pass it in, set on the parsed
/// args: the live dispatch reads the same field from the schema's `strict` key.
pub fn edit(
    cwd: &Path,
    args: &serde_json::Value,
    registry: Option<&mut hashline::registry::Registry>,
    strict: bool,
) -> ToolResult {
    match EditArgs::parse(args) {
        Ok(mut a) => {
            a.strict = strict;
            edit_impl(cwd, a, registry)
        }
        Err(e) => ToolResult::err(e.into()),
    }
}

pub async fn bash_inner(
    cwd: &Path,
    command: &str,
    args: &serde_json::Value,
    stop: &AtomicBool,
    progress: Option<&crate::tools::ToolProgress>,
) -> ToolResult {
    let mut json = args.clone();
    json["command"] = serde_json::Value::String(command.to_owned());
    match BashArgs::parse(&json) {
        Ok(a) => bash_inner_impl(cwd, &a, stop, progress).await,
        Err(e) => ToolResult::err(e.into()),
    }
}
/// A throwaway session registry for a test, locked for the call.
pub fn fresh_registry() -> parking_lot::MutexGuard<'static, hashline::registry::Registry> {
    // A 'static guard is not possible; these tests own the registry for the
    // duration of the call, so a leaked-once box is acceptable in test code.
    let box_ = Box::leak(Box::new(parking_lot::Mutex::new(
        hashline::registry::Registry::new(),
    )));
    box_.lock()
}

/// The text of a tool output: these tests assert on text, and an image
/// block on these paths is a bug.
pub fn text(out: ToolResult) -> String {
    match out.into_parts().0 {
        ToolOutput::Text(t) => t,
        ToolOutput::Image { .. } => panic!("expected text, got an image block"),
    }
}

/// The GUI-only detail channel's text (#243): an `edit`'s diff, a `write`'
/// content, a `read`'s anchor-stripped body. `None` on non-split results.
pub fn detail(out: ToolResult) -> Option<String> {
    match out.into_parts().1 {
        Some(ToolOutput::Text(t)) => Some(t),
        Some(ToolOutput::Image { .. }) => panic!("expected text detail, got an image block"),
        None => None,
    }
}

#[tokio::test]
async fn write_then_read_roundtrip_with_anchors() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        &json!({"path": "a.txt", "content": "one\ntwo\n"}),
        None,
    )
    .await;
    let mut reg = fresh_registry();
    let out = text(read(
        dir.path(),
        &json!({"path": "a.txt"}),
        None,
        None,
        Some(&mut *reg),
    ));
    let rows: Vec<&str> = out.lines().collect();
    assert_eq!(rows.len(), 2);
    assert!(rows[0].ends_with("one"));
    assert!(rows[1].ends_with("two"));
    let from = rows[0].split('│').next().unwrap().to_string();
    let to = rows[1].split('│').next().unwrap().to_string();
    let res = edit(
        dir.path(),
        &json!({"path": "a.txt", "edits": [{"from": from, "to": to, "content": "ONE AND TWO"}]}),
        Some(&mut *reg),
        false,
    );
    // #243: the model sees the one-line ack, not the diff.
    assert!(
        text(res.clone()).starts_with("Successfully edited "),
        "edit failed: {res:?}"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
        "ONE AND TWO\n"
    );
}

#[tokio::test]
async fn bash_timeout_kills_the_child_not_just_the_wait() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("marker");
    let out = text(
        bash(
            dir.path(),
            &json!({"command": "sleep 5; touch marker", "timeout_secs": 1}),
        )
        .await,
    );
    assert!(out.contains("timed out after 1s"), "{out}");
    // If the child survived the timeout it would finish its sleep and
    // write the marker; give it time to prove it is dead.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert!(!marker.exists(), "the timed-out child kept running");
}

#[tokio::test]
async fn bash_captures_output_and_exit_code() {
    let dir = tempfile::tempdir().unwrap();
    let out = text(bash(dir.path(), &json!({"command": "echo hello"})).await);
    assert!(out.starts_with("exit 0"), "{out}");
    assert!(out.contains("hello"), "{out}");
    let out = text(bash(dir.path(), &json!({"command": "echo oops >&2; exit 3"})).await);
    assert!(out.starts_with("exit 3"), "{out}");
    assert!(out.contains("oops"), "{out}");
}

#[tokio::test]
async fn bash_child_path_includes_common_tool_dirs() {
    let dir = tempfile::tempdir().unwrap();
    let out = text(bash(dir.path(), &json!({"command": "echo $PATH"})).await);
    // bash_path() augments the GUI-minimal inherited PATH with the common
    // per-user tool dirs, so bare commands resolve (#172).
    assert!(out.contains("/usr/local/bin"), "{out}");
    assert!(out.contains("/opt/homebrew/bin"), "{out}");
}

#[tokio::test]
async fn bash_stop_interrupts_a_running_command() {
    let dir = tempfile::tempdir().unwrap();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stopper = std::sync::Arc::clone(&stop);
    let setter = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        stopper.store(true, std::sync::atomic::Ordering::SeqCst);
    });
    let started = std::time::Instant::now();
    let out = text(bash_inner(dir.path(), "sleep 10", &json!({}), &stop, None).await);
    let elapsed = started.elapsed();
    // The stop cut the command short, not the sleep running to completion (#174).
    assert!(out.contains("cancelled by user stop"), "{out}");
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "stop did not interrupt in time: {elapsed:?}"
    );
    setter.await.unwrap();
}

#[tokio::test]
async fn core_tools_carry_a_typed_verdict() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path();
    // bash: the exit code is the verdict.
    let ok = bash(cwd, &json!({"command": "true"})).await;
    assert_eq!(ok.status(), ToolStatus::Ok);
    let err = bash(cwd, &json!({"command": "false"})).await;
    assert_eq!(err.status(), ToolStatus::Error);
    // read: a missing file is an error; a present one is ok.
    let err = read(cwd, &json!({"path": "missing.txt"}), None, None, None);
    assert_eq!(err.status(), ToolStatus::Error);
    std::fs::write(cwd.join("a.txt"), "hi\n").unwrap();
    let ok = read(cwd, &json!({"path": "a.txt"}), None, None, None);
    assert_eq!(ok.status(), ToolStatus::Ok);
    // write: a clean write is ok.
    let ok = write(cwd, &json!({"path": "b.txt", "content": "x"}), None).await;
    assert_eq!(ok.status(), ToolStatus::Ok);
}

#[tokio::test]
async fn write_ack_carries_byte_and_line_counts() {
    let dir = tempfile::tempdir().unwrap();
    // 7 bytes, 2 lines (\n terminates the first).
    let out = text(
        write(
            dir.path(),
            &json!({"path": "counted.txt", "content": "one\ntwo"}),
            None,
        )
        .await,
    );
    assert_eq!(
        out,
        format!(
            "wrote {} (7 bytes, 2 lines)",
            dir.path().join("counted.txt").display()
        )
    );
    // An empty write is 0 bytes and 0 lines.
    let out = text(
        write(
            dir.path(),
            &json!({"path": "empty.txt", "content": ""}),
            None,
        )
        .await,
    );
    assert_eq!(
        out,
        format!(
            "wrote {} (0 bytes, 0 lines)",
            dir.path().join("empty.txt").display()
        )
    );
}

#[tokio::test]
async fn write_missing_args_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let err = write(dir.path(), &json!({"content": "x"}), None).await;
    assert_eq!(err.status(), ToolStatus::Error);
    assert!(
        text(err).contains("missing \"path\""),
        "wrong failure: expected missing path"
    );
    let err = write(dir.path(), &json!({"path": "x.txt"}), None).await;
    assert_eq!(err.status(), ToolStatus::Error);
    assert!(
        text(err).contains("missing \"content\""),
        "wrong failure: expected missing content"
    );
}

#[tokio::test]
async fn write_ui_detail_carries_the_written_content() {
    // #243: the model gets the #184 ack; the echoed content is UI-only.
    let dir = tempfile::tempdir().unwrap();
    let res = write(
        dir.path(),
        &json!({"path": "w.txt", "content": "alpha\nbeta\n"}),
        None,
    )
    .await;
    assert_eq!(
        text(res.clone()),
        format!(
            "wrote {} (11 bytes, 3 lines)",
            dir.path().join("w.txt").display()
        )
    );
    assert_eq!(detail(res).as_deref(), Some("alpha\nbeta\n"));
}

#[tokio::test]
async fn bash_and_errors_carry_no_ui_detail() {
    // #243 negative: a non-split result keeps the channel absent — the wire
    // line stays byte-identical to a pre-#243 record.
    let dir = tempfile::tempdir().unwrap();
    let res = bash(dir.path(), &json!({"command": "echo hi"})).await;
    assert!(res.ui_detail().is_none(), "bash must not split: {res:?}");
    let err = write(dir.path(), &json!({"content": "x"}), None).await;
    assert!(
        err.ui_detail().is_none(),
        "an error is single-channel: {err:?}"
    );
}

#[cfg(unix)]
mod stdbuf {
    use super::impls::{bash_argv, stdbuf_in_path};

    #[test]
    fn stdbuf_wrap_produces_line_buffered_argv() {
        // #249: when stdbuf is available the command runs under it so stdout/
        // stderr are line-buffered, not block-buffered on a pipe.
        let argv = bash_argv("echo hi", true);
        assert_eq!(
            argv,
            vec![
                "stdbuf".to_string(),
                "-oL".to_string(),
                "-eL".to_string(),
                "bash".to_string(),
                "-c".to_string(),
                "echo hi".to_string()
            ]
        );
    }

    #[test]
    fn no_stdbuf_falls_back_to_plain_bash() {
        // #249 negative: without stdbuf the command runs as plain bash (the
        // pre-#249 argv) — silent degradation, not a failure.
        let argv = bash_argv("echo hi", false);
        assert_eq!(
            argv,
            vec!["bash".to_string(), "-c".to_string(), "echo hi".to_string()]
        );
    }

    #[test]
    fn stdbuf_probe_reports_presence() {
        // #249: the probe is pure over the PATH string, so the cases are
        // built, not assumed from the host (a CI macOS has no GNU stdbuf).
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("stdbuf"), b"").unwrap();
        assert!(stdbuf_in_path(&dir.path().to_string_lossy()));
        assert!(!stdbuf_in_path("/nonexistent"));
    }
}
