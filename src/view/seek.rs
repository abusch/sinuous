//! Seeking in the current track, showing where to until the speakers get there.

use std::time::{Duration, Instant};

use sinuous_client::{GroupId, playback::PlaybackState};

use crate::sonos::{GroupState, HouseholdState};

/// How far `<` and `>` seek.
pub(super) const SEEK_STEP: Duration = Duration::from_secs(10);
/// How long after the last seek to stop showing where it goes if the speakers don't report getting
/// there, e.g. because it failed.
const TIMEOUT: Duration = Duration::from_secs(3);
/// How far from where a seek goes the speakers can report being once they got there.
const TOLERANCE: Duration = Duration::from_secs(2);

/// A seek sent to the speakers, whose target is shown in place of the position they report until
/// they get there.
#[derive(Debug, Clone)]
pub(super) struct PendingSeek {
    group: GroupId,
    target: Duration,
    /// When the last seek was sent.
    sent: Instant,
}

impl PendingSeek {
    /// The seek [`SEEK_STEP`] `forward`, or back, in the track `group` plays at `now`: from where
    /// `previous` goes if it seeks in the same group, so that pressing a key again goes further,
    /// or from where the group is.
    pub(super) fn step(
        previous: Option<&Self>,
        group: &GroupState,
        forward: bool,
        now: Instant,
    ) -> Option<Self> {
        let from = match previous {
            Some(previous) if previous.group == group.id => previous.target,
            _ => group.playback?.position(now),
        };
        let mut target = if forward {
            from + SEEK_STEP
        } else {
            from.saturating_sub(SEEK_STEP)
        };
        if let Some(duration) = group.now_playing.as_ref().and_then(|t| t.duration_secs) {
            target = target.min(Duration::from_secs(duration.into()));
        }
        Some(Self {
            group: group.id.clone(),
            target,
            sent: now,
        })
    }

    pub(super) fn target(&self) -> Duration {
        self.target
    }

    /// Where `group` is seeking to, if this seek is in it.
    pub(super) fn target_in(&self, group: &GroupState) -> Option<Duration> {
        (self.group == group.id).then_some(self.target)
    }

    /// Whether this seek is in `group`.
    pub(super) fn is_in(&self, group: &GroupId) -> bool {
        self.group == *group
    }

    /// Whether the speakers are still to report getting where the seek goes, in `state` at `now`.
    pub(super) fn is_pending(&self, state: &HouseholdState, now: Instant) -> bool {
        if now.saturating_duration_since(self.sent) > TIMEOUT {
            return false;
        }
        let Some(group) = state.group(&self.group) else {
            return false;
        };
        let arrived = group.playback.is_some_and(|playback| {
            playback.state != PlaybackState::Buffering
                && playback.reported_position().abs_diff(self.target) <= TOLERANCE
        });
        !arrived
    }
}

#[cfg(test)]
mod tests {
    use sinuous_client::playback::PlaybackActions;

    use super::*;
    use crate::sonos::{Playback, TrackInfo};
    use crate::view::tests::household;

    fn at(state: PlaybackState, secs: u64) -> Option<Playback> {
        Some(Playback::at(
            state,
            Duration::from_secs(secs),
            PlaybackActions::default(),
        ))
    }

    /// The household, with its first group paused this far into a track lasting 3 minutes.
    fn paused_at(secs: u64) -> HouseholdState {
        let mut state = household();
        let group = &mut state.groups[0];
        group.playback = at(PlaybackState::Paused, secs);
        group.now_playing = Some(TrackInfo {
            title: Some("Nocturne".to_owned()),
            artist: Some("TesseracT".to_owned()),
            explicit: false,
            duration_secs: Some(180),
            image_url: None,
            is_station: false,
        });
        state
    }

    #[test]
    fn steps_go_further_from_the_last_target() {
        let state = paused_at(60);
        let group = &state.groups[0];
        let now = Instant::now();

        let seek = PendingSeek::step(None, group, true, now).unwrap();
        assert_eq!(seek.target(), Duration::from_secs(70));
        let seek = PendingSeek::step(Some(&seek), group, true, now).unwrap();
        assert_eq!(seek.target(), Duration::from_secs(80));
        let seek = PendingSeek::step(Some(&seek), group, false, now).unwrap();
        assert_eq!(seek.target(), Duration::from_secs(70));

        // A seek in another group doesn't count.
        let other = PendingSeek::step(Some(&seek), &state.groups[1], true, now);
        assert!(other.is_none(), "the other group plays nothing");
    }

    #[test]
    fn steps_stay_within_the_track() {
        let now = Instant::now();
        let state = paused_at(5);
        let seek = PendingSeek::step(None, &state.groups[0], false, now).unwrap();
        assert_eq!(seek.target(), Duration::ZERO);
        let state = paused_at(175);
        let seek = PendingSeek::step(None, &state.groups[0], true, now).unwrap();
        assert_eq!(seek.target(), Duration::from_secs(180));
    }

    #[test]
    fn seeks_are_pending_until_the_speakers_get_there_or_for_a_while() {
        let mut state = paused_at(60);
        let now = Instant::now();
        let seek = PendingSeek::step(None, &state.groups[0], true, now).unwrap();
        // The speakers haven't reported anything new yet.
        assert!(seek.is_pending(&state, now));
        state.groups[0].playback = at(PlaybackState::Buffering, 70);
        assert!(seek.is_pending(&state, now));
        state.groups[0].playback = at(PlaybackState::Playing, 70);
        assert!(!seek.is_pending(&state, now));

        let state = paused_at(60);
        assert!(seek.is_pending(&state, now + TIMEOUT));
        assert!(!seek.is_pending(&state, now + TIMEOUT + Duration::from_millis(1)));
    }
}
