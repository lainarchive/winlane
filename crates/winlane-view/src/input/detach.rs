use crate::input::{Key, KeyChord};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetachDecision {
    NotDetach,
    Armed,
    Confirmed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetachGuard {
    binding: KeyChord,
    armed: bool,
}

impl Default for DetachGuard {
    fn default() -> Self {
        Self::new(KeyChord::ctrl(Key::Char('q')))
    }
}

impl DetachGuard {
    pub fn new(binding: KeyChord) -> Self {
        Self {
            binding,
            armed: false,
        }
    }

    pub fn binding(&self) -> &KeyChord {
        &self.binding
    }

    pub fn observe(&mut self, chord: &KeyChord) -> DetachDecision {
        if chord != &self.binding {
            self.armed = false;
            return DetachDecision::NotDetach;
        }
        if self.armed {
            self.armed = false;
            DetachDecision::Confirmed
        } else {
            self.armed = true;
            DetachDecision::Armed
        }
    }

    pub fn reset(&mut self) {
        self.armed = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detach_requires_two_consecutive_binding_presses() {
        let mut guard = DetachGuard::default();
        let binding = KeyChord::ctrl(Key::Char('q'));
        assert_eq!(guard.observe(&binding), DetachDecision::Armed);
        assert_eq!(guard.observe(&binding), DetachDecision::Confirmed);
    }

    #[test]
    fn other_input_disarms_detach() {
        let mut guard = DetachGuard::default();
        let binding = KeyChord::ctrl(Key::Char('q'));
        assert_eq!(guard.observe(&binding), DetachDecision::Armed);
        assert_eq!(
            guard.observe(&KeyChord::plain(Key::Char('x'))),
            DetachDecision::NotDetach
        );
        assert_eq!(guard.observe(&binding), DetachDecision::Armed);
    }
}
