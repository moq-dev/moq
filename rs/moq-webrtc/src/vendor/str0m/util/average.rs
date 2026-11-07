/// Exponential moving average
#[derive(Debug)]
pub struct MovingAverage {
    smoothing_factor: f64,
    average: Option<f64>,
    variance: f64,
    std: f64,
}

impl MovingAverage {
    pub fn new(smoothing_factor: f64) -> Self {
        Self {
            smoothing_factor,
            average: None,
            variance: 0.0,
            std: 0.0,
        }
    }

    pub fn within_std(&self, value: f64, num_std: f64) -> bool {
        let Some(average) = self.average else {
            return false;
        };

        let floor = average - self.std * num_std;
        let ceil = average + self.std * num_std;

        floor <= value && value <= ceil
    }

    pub fn upper_range(&self, num_std: f64) -> Option<f64> {
        if self.std == 0.0 {
            return None;
        }

        self.average.map(|avg| avg + num_std * self.std)
    }

    pub fn lower_range(&self, num_std: f64) -> Option<f64> {
        if self.std == 0.0 {
            return None;
        }

        self.average.map(|avg| avg - num_std * self.std)
    }

    pub fn update(&mut self, value: f64) {
        let average = match self.average {
            Some(average) => {
                let delta = value - average;
                let new_average = average + self.smoothing_factor * delta;
                let new_variance = (1.0 - self.smoothing_factor)
                    * (self.variance + self.smoothing_factor * delta.powf(2.0));

                self.variance = new_variance;
                self.std = new_variance.sqrt();

                new_average
            }
            None => value,
        };

        self.average = Some(average);
    }

    /// Returns the current average value, or None if no values have been added yet.
    pub fn get(&self) -> Option<f64> {
        self.average
    }

    pub fn valid(&self) -> bool {
        self.average.is_some()
    }

    pub fn reset(&mut self) {
        self.average = None;
        self.std = 0.0;
        self.variance = 0.0;
    }
}
