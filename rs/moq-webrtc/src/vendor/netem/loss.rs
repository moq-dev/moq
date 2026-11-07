use fastrand::Rng;

use crate::config::{GilbertElliot, LossModel, RandomLoss};

/// State for the loss model evaluation.
#[derive(Debug, Clone)]
pub(crate) enum LossState {
    /// No loss - no state needed.
    None,

    /// State for random loss with correlation.
    Random {
        /// Last random value for correlated loss.
        last_random: f32,
    },

    /// State for Gilbert-Elliot loss model.
    GilbertElliot {
        /// Current state in the Markov chain (true = Good, false = Bad).
        in_good_state: bool,
    },
}

impl LossState {
    pub fn new(model: &LossModel) -> Self {
        match model {
            LossModel::None => LossState::None,
            LossModel::Random(_) => LossState::Random { last_random: 0.0 },
            LossModel::GilbertElliot(_) => LossState::GilbertElliot {
                in_good_state: true,
            },
        }
    }

    /// Evaluate whether a packet should be lost.
    ///
    /// Returns `true` if the packet should be dropped.
    pub fn should_lose(&mut self, model: &LossModel, rng: &mut Rng) -> bool {
        match (self, model) {
            (LossState::None, LossModel::None) => false,
            (LossState::Random { last_random }, LossModel::Random(random)) => {
                evaluate_random(last_random, random, rng)
            }
            (LossState::GilbertElliot { in_good_state }, LossModel::GilbertElliot(ge)) => {
                evaluate_gilbert_elliot(in_good_state, ge, rng)
            }
            _ => false,
        }
    }
}

/// Evaluate the random loss model with optional correlation.
fn evaluate_random(last_random: &mut f32, random: &RandomLoss, rng: &mut Rng) -> bool {
    let rho = random.correlation;

    let value = if rho == 0.0 {
        let v = rng.f32();
        *last_random = v;
        v
    } else {
        let fresh = rng.f32();
        let v = fresh * (1.0 - rho) + *last_random * rho;
        *last_random = v;
        v
    };

    value < random.probability
}

/// Evaluate the Gilbert-Elliot loss model.
///
/// First, potentially transition between GOOD and BAD states.
/// Then, decide whether to lose the packet based on current state.
fn evaluate_gilbert_elliot(in_good_state: &mut bool, ge: &GilbertElliot, rng: &mut Rng) -> bool {
    // State transition
    if *in_good_state {
        if rng.f32() < ge.p {
            *in_good_state = false;
        }
    } else if rng.f32() < ge.r {
        *in_good_state = true;
    }

    // Loss decision based on current state
    if *in_good_state {
        rng.f32() < ge.k
    } else {
        rng.f32() < ge.h
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Probability;

    #[test]
    fn test_no_loss() {
        let model = LossModel::None;
        let mut state = LossState::new(&model);
        let mut rng = Rng::with_seed(42);

        for _ in 0..100 {
            assert!(!state.should_lose(&model, &mut rng));
        }
    }

    #[test]
    fn test_random_loss_zero() {
        let model = LossModel::Random(RandomLoss::new(Probability::ZERO));
        let mut state = LossState::new(&model);
        let mut rng = Rng::with_seed(42);

        for _ in 0..100 {
            assert!(!state.should_lose(&model, &mut rng));
        }
    }

    #[test]
    fn test_random_loss_one() {
        let model = LossModel::Random(RandomLoss::new(Probability::ONE));
        let mut state = LossState::new(&model);
        let mut rng = Rng::with_seed(42);

        for _ in 0..100 {
            assert!(state.should_lose(&model, &mut rng));
        }
    }

    #[test]
    fn test_random_loss_half() {
        let model = LossModel::Random(RandomLoss::new(Probability::new(0.5)));
        let mut state = LossState::new(&model);
        let mut rng = Rng::with_seed(42);

        let mut lost = 0;
        let total = 10000;

        for _ in 0..total {
            if state.should_lose(&model, &mut rng) {
                lost += 1;
            }
        }

        // Should be roughly 50% with some tolerance
        let ratio = lost as f32 / total as f32;
        assert!((0.45..=0.55).contains(&ratio), "Loss ratio: {}", ratio);
    }

    #[test]
    fn test_gilbert_elliot_wifi() {
        let model = LossModel::GilbertElliot(GilbertElliot::wifi());
        let mut state = LossState::new(&model);
        let mut rng = Rng::with_seed(42);

        let mut lost = 0;
        let total = 10000;

        for _ in 0..total {
            if state.should_lose(&model, &mut rng) {
                lost += 1;
            }
        }

        // WiFi preset should be around 1% loss
        let ratio = lost as f32 / total as f32;
        assert!((0.005..=0.03).contains(&ratio), "Loss ratio: {}", ratio);
    }
}
