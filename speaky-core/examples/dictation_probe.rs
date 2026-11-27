//! JSON-lines driver for the secondary-monitor regression script; never opens a window.
#[cfg(windows)]
fn main() {
    use speaky_core::dictation_target::{self, DictationTarget, Foreground};
    use std::io::{self, BufRead, Write};
    let mut live = None;
    let mut stopped = None;
    for line in io::stdin().lock().lines().map_while(Result::ok) {
        let request: serde_json::Value = serde_json::from_str(&line).unwrap();
        let text = request["text"].as_str().unwrap_or_default();
        let result = match request["op"].as_str().unwrap_or_default() {
            "start" => match DictationTarget::capture_foreground() {
                Foreground::External(Some(target)) => {
                    live = Some(target);
                    stopped = None;
                    Ok(())
                }
                _ => Err("No external editor".into()),
            },
            "preview" => live
                .as_ref()
                .ok_or_else(|| "No live target".into())
                .map(|target| target.preview(text)),
            "cancel" => live
                .as_ref()
                .ok_or_else(|| "No live target".into())
                .and_then(DictationTarget::cancel_preview),
            "stop" => {
                DictationTarget::capture_at_stop(live.as_ref()).map(|target| stopped = Some(target))
            }
            "deliver" => stopped
                .as_ref()
                .ok_or_else(|| "No stopped target".into())
                .and_then(|target| target.deliver(text)),
            "revise" => stopped
                .as_ref()
                .ok_or_else(|| "No stopped target".into())
                .and_then(|target| target.revise(text)),
            "fallback" => dictation_target::copy_fallback(text),
            "status" => live
                .as_ref()
                .and_then(DictationTarget::delivery_issue)
                .map_or(Ok(()), Err),
            _ => Err("Unknown operation".into()),
        };
        println!(
            "{}",
            match result {
                Ok(()) => serde_json::json!({"ok": true}),
                Err(error) => serde_json::json!({"ok": false, "error": error}),
            }
        );
        io::stdout().flush().unwrap();
    }
}
#[cfg(not(windows))]
fn main() {
    eprintln!("The dictation probe requires Windows.");
}
