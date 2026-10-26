//! Limiteur à anticipation, partagé par la voix et les sons.

/// Plafond qui voit quelques millisecondes en avance : le gain descend en
/// douceur avant une crête au lieu de la couper net (une coupure nette
/// déforme l'onde : grésillement sur les éclats de voix, les R roulés, les
/// basses fortes). Le même gain s'applique à tous les canaux, pour ne pas
/// décaler l'image stéréo.
pub struct LookaheadLimiter {
    channels: usize,
    ceiling: f32,
    release: f32,
    /// Trames retardées (entrelacées), anneau de `lookahead` trames.
    delay: Vec<f32>,
    at_delay: usize,
    /// Besoin de gain de chaque trame et minima glissants (anneaux de
    /// `lookahead + 1`).
    need: Vec<f32>,
    mins: Vec<f32>,
    sum: f64,
    at: usize,
    gain: f32,
}

impl LookaheadLimiter {
    /// `release` : coefficient de remontée du gain par trame (proche de 1).
    pub fn new(channels: usize, lookahead: usize, ceiling: f32, release: f32) -> Self {
        let channels = channels.max(1);
        let lookahead = lookahead.max(1);
        let n = lookahead + 1;
        Self {
            channels,
            ceiling,
            release,
            delay: vec![0.0; lookahead * channels],
            at_delay: 0,
            need: vec![1.0; n],
            mins: vec![1.0; n],
            sum: n as f64,
            at: 0,
            gain: 1.0,
        }
    }

    /// Retard ajouté, en trames.
    pub fn latency(&self) -> usize {
        self.delay.len() / self.channels
    }

    /// Remplace `frame` par la trame d'il y a `latency()` trames, plafonnée.
    /// Chaque gain est la moyenne de minima qui couvrent tous la crête à
    /// venir : jamais au-dessus du plafond, et sans marche d'escalier.
    pub fn process(&mut self, frame: &mut [f32]) {
        let ch = self.channels;
        let peak = frame.iter().take(ch).fold(0.0f32, |m, v| m.max(v.abs()));
        let slot = &mut self.delay[self.at_delay * ch..(self.at_delay + 1) * ch];
        for (d, f) in slot.iter_mut().zip(frame.iter_mut()) {
            std::mem::swap(d, f);
        }
        self.at_delay = (self.at_delay + 1) % self.latency();

        let n = self.need.len();
        self.need[self.at] = if peak > self.ceiling { self.ceiling / peak } else { 1.0 };
        let min = self.need.iter().fold(1.0f32, |m, &g| m.min(g));
        self.sum += min as f64 - self.mins[self.at] as f64;
        self.mins[self.at] = min;
        self.at = (self.at + 1) % n;
        let wanted = (self.sum / n as f64) as f32;
        self.gain = if wanted < self.gain { wanted } else { wanted + (self.gain - wanted) * self.release };
        for f in frame.iter_mut().take(ch) {
            *f *= self.gain;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stereo_peaks_never_exceed_ceiling() {
        let mut l = LookaheadLimiter::new(2, 96, 0.5, 0.999);
        let mut peak = 0.0f32;
        for i in 0..20_000 {
            let loud = if (5000..6000).contains(&i) { 1.8 } else { 0.3 };
            let x = (i as f32 * 0.05).sin() * loud;
            let mut frame = [x, -x * 0.5];
            l.process(&mut frame);
            peak = peak.max(frame[0].abs()).max(frame[1].abs());
        }
        assert!(peak <= 0.5 + 1e-4, "peak {peak}");
    }
}
