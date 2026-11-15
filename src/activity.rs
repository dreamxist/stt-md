//! Telling macOS the app is busy.
//!
//! A menubar app with no window is the first thing App Nap puts to sleep: its
//! timers stretch to several seconds and its threads lose priority. That is
//! fine while idle and wrong while a meeting is being recorded or transcribed,
//! so those stretches are declared as user-initiated activity, which also
//! keeps the Mac from idle-sleeping in the middle of them.

use objc2::rc::Retained;
use objc2::runtime::{NSObjectProtocol, ProtocolObject};
use objc2_foundation::{NSActivityOptions, NSProcessInfo, NSString};

/// Held for as long as the work lasts; dropping it ends the activity.
pub struct KeepAwake {
    token: Retained<ProtocolObject<dyn NSObjectProtocol>>,
}

impl KeepAwake {
    pub fn begin(reason: &str) -> Self {
        let token = NSProcessInfo::processInfo().beginActivityWithOptions_reason(
            NSActivityOptions::UserInitiated,
            &NSString::from_str(reason),
        );
        Self { token }
    }
}

impl Drop for KeepAwake {
    fn drop(&mut self) {
        // SAFETY: `token` is what `beginActivityWithOptions:reason:` returned.
        unsafe { NSProcessInfo::processInfo().endActivity(&self.token) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_activity_begins_and_ends_without_complaint() {
        let first = KeepAwake::begin("test");
        let second = KeepAwake::begin("test, nested");
        drop(first);
        drop(second);
    }
}
