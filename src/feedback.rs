//! Audio feedback for recording state changes.

use std::process::Command;

const SOUND_START: &str = "/usr/share/sounds/freedesktop/stereo/device-added.oga";
const SOUND_STOP: &str = "/usr/share/sounds/freedesktop/stereo/complete.oga";

pub fn play_start_sound() {
    play_sound(SOUND_START);
}

pub fn play_stop_sound() {
    play_sound(SOUND_STOP);
}

fn play_sound(path: &str) {
    // Spawn paplay in background, ignore errors (sound is optional)
    let _ = Command::new("paplay").arg(path).spawn();
}
