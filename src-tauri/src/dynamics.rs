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
    /// 8 dernières valeurs de chaque canal, pour estimer les crêtes entre
    /// deux échantillons (« true peak »).
    history: Vec<[f32; TAPS]>,
}

const TAPS: usize = 8;
/// Positions entre deux échantillons où l'onde est reconstruite (×4).
const PHASES: [f32; 3] = [0.25, 0.5, 0.75];

/// Coefficients d'interpolation (sinus cardinal fenêtré, 8 points) pour
/// chaque position intermédiaire, entre les échantillons 3 et 4.
fn true_peak_coefs() -> &'static [[f32; TAPS]; 3] {
    static COEFS: std::sync::OnceLock<[[f32; TAPS]; 3]> = std::sync::OnceLock::new();
    COEFS.get_or_init(|| {
        let mut out = [[0.0f32; TAPS]; 3];
        for (row, &frac) in out.iter_mut().zip(PHASES.iter()) {
            let mut sum = 0.0f32;
            for (k, c) in row.iter_mut().enumerate() {
                // Distance au point à reconstruire (situé à 3 + frac).
                let x = k as f32 - 3.0 - frac;
                let sinc = if x.abs() < 1e-6 { 1.0 } else { (std::f32::consts::PI * x).sin() / (std::f32::consts::PI * x) };
                // Fenêtre de Hann sur ±4 échantillons.
                let w = 0.5 + 0.5 * (std::f32::consts::PI * x / 4.0).cos();
                *c = sinc * w;
                sum += *c;
            }
            row.iter_mut().for_each(|c| *c /= sum);
        }
        out
    })
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
            history: vec![[0.0; TAPS]; channels],
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
        // Crête vraie : l'échantillon lui-même et l'onde reconstruite entre
        // les échantillons voisins, là où le décodage de Discord (Opus) et
        // les cartes son peuvent dépasser le plafond.
        let coefs = true_peak_coefs();
        let mut peak = 0.0f32;
        for (h, &v) in self.history.iter_mut().zip(frame.iter().take(ch)) {
            h.copy_within(1.., 0);
            h[TAPS - 1] = v;
            peak = peak.max(v.abs());
            for row in coefs {
                let y: f32 = row.iter().zip(h.iter()).map(|(c, x)| c * x).sum();
                peak = peak.max(y.abs());
            }
        }
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
    fn inter_sample_peaks_are_caught() {
        // Sinus au quart de la fréquence d'échantillonnage, déphasé de 45° :
        // les échantillons valent ±0,707 mais l'onde monte à 1,0.
        let mut l = LookaheadLimiter::new(1, 96, 0.8, 0.999);
        let mut out = Vec::new();
        for i in 0..4000 {
            let mut f = [(std::f32::consts::FRAC_PI_2 * i as f32 + std::f32::consts::FRAC_PI_4).sin()];
            l.process(&mut f);
            out.push(f[0]);
        }
        // Sans « true peak », le gain resterait à 1 (0,707 < 0,8).
        let tail = &out[2000..];
        let sample_peak = tail.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(sample_peak < 0.62, "pic {sample_peak}");
    }

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
