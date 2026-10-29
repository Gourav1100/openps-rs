//! 16-bit id arithmetic used by the video/audio/haptic stages. Quirk reproduced on purpose: circular
//! comparisons use modulus **65535**, not 65536, while id increments wrap at 65536 (spec 02 R2).

pub const MOD: i32 = 65535;

pub fn modp(x: i32) -> i32 {
    x.rem_euclid(MOD)
}

/// `id` is at or behind `last` in circular order (stale).
pub fn is_older(last: i32, id: i32) -> bool {
    modp(last - id) <= modp(id - last)
}

/// `id` has reached `limit` (== or ahead of it).
pub fn reached(id: i32, limit: i32) -> bool {
    id == limit || modp(limit - id) > modp(id - limit)
}

/// `id + 1` with the 16-bit wrap used by the cursor (`> 65535 -> -65536`).
pub fn next_id(id: i32) -> i32 {
    let n = id + 1;
    if n > 65535 {
        n - 65536
    } else {
        n
    }
}

/// Freshness filter shared by audio and haptics (`gr`): tracks the last accepted id and the next expected one.
#[derive(Clone, Debug)]
pub struct SeqFilter {
    last: i32,
    next: i32,
}

impl Default for SeqFilter {
    fn default() -> Self {
        Self { last: -1, next: -1 }
    }
}

impl SeqFilter {
    pub fn is_stale(&self, id: i32) -> bool {
        if id == self.next || self.next == -1 {
            false
        } else {
            self.last == id || is_older(self.last, id)
        }
    }
    pub fn accept(&mut self, id: i32) {
        self.last = id;
        self.next = next_id(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn circular_compare_uses_65535() {
        assert!(is_older(100, 100));
        assert!(is_older(100, 50));
        assert!(!is_older(100, 150));
        assert!(is_older(5, 65530)); // just before the wrap counts as older
        assert!(!is_older(65530, 5)); // just after the wrap is newer
        assert!(reached(10, 10) && reached(11, 10) && !reached(9, 10));
        assert!(reached(2, 65530)); // wrapped ahead
    }

    #[test]
    fn id_increment_wraps_at_65536() {
        assert_eq!(next_id(65535), 0);
        assert_eq!(next_id(7), 8);
    }

    #[test]
    fn filter_accepts_new_and_rejects_repeats() {
        let mut f = SeqFilter::default();
        assert!(!f.is_stale(500)); // first ever
        f.accept(500);
        assert!(!f.is_stale(501)); // expected next
        assert!(f.is_stale(500));
        assert!(f.is_stale(499));
        assert!(!f.is_stale(510)); // ahead (gap)
        f.accept(510);
        assert!(f.is_stale(505));
    }
}
