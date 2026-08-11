//! Who gets to see the screen, and which one.
//!
//! This is the question the whole portal exists to ask. An application does not
//! reach the compositor's capture protocol directly — it asks here, and here is
//! where the user is put in front of it.
//!
//! ## Why the question is asked somewhere else
//!
//! This process cannot draw. The shell owns the renderer, the glass, the fonts
//! and the centred panel every other question in this session is asked through,
//! and a portal that grew a second toolkit to draw one dialog would be a second
//! desktop. So the question goes over `lxb_shell_v1`, the session's own
//! channel: the compositor carries it to whichever shell is up — over the top
//! of a fullscreen application, which is the one case that matters — and
//! carries the answer back.
//!
//! ## Everything that is not a yes is a no
//!
//! A refusal, a shell that never answers, a session with no shell at all, a
//! compositor that is not LineXinBar: all of them come back as `None` and are
//! answered up the chain as the user saying no. There is no path through here
//! that shares a screen because something was missing.

/// How long an unanswered question is left standing.
///
/// Long enough for somebody to walk back to the machine and read it, short
/// enough that an application asking into a session nobody is at eventually
/// gets an answer rather than hanging for ever. A shell that is up answers as
/// soon as the user presses something; this is only the ceiling.
const PATIENCE: std::time::Duration = std::time::Duration::from_secs(120);

/// Which display an application may see, if any.
///
/// `None` is the user saying no, which is answered up the chain as a refusal
/// rather than as a failure: an application that is told no is expected to
/// carry on.
pub fn ask(app_id: &str, displays: &[String]) -> Option<String> {
    let application = if app_id.trim().is_empty() {
        "an application"
    } else {
        app_id
    };
    tracing::info!(%application, "asking whether the screen may be shared");

    let answer = match crate::cast::ask_to_share(app_id, PATIENCE) {
        Ok(answer) => answer,
        Err(err) => {
            // Nobody to ask is not permission to go ahead.
            tracing::warn!(?err, %application, "could not ask; refusing");
            return None;
        }
    };

    let chosen = answer?;
    // The shell answers with a display; that it is one of the displays this
    // portal can actually cast is worth checking rather than assuming, because
    // a screen can be unplugged between the question and the answer.
    if !displays.contains(&chosen) {
        tracing::warn!(%chosen, "the display chosen is no longer there");
        return None;
    }
    tracing::info!(%application, display = %chosen, "the user allowed a screen to be shared");
    Some(chosen)
}
