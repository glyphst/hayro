//! Shared fixed-output policy for PDF CIE-based source color spaces.
use hayro_syntax::object::{Array, Dict};
use moxcms::{Matrix3d, Vector3d, Xyz, adaption_matrix_d};

#[derive(Debug, Clone)]
pub(super) struct CieRgb {
    matrix: Matrix3d,
    offset: Vector3d,
}

impl CieRgb {
    pub(super) fn new(white: [f32; 3], black: [f32; 3]) -> Option<Self> {
        if white[0] <= 0.0
            || white[1] != 1.0
            || white[2] <= 0.0
            || black.iter().any(|value| *value < 0.0)
            || black == white
        {
            return None;
        }
        let d65 = Xyz {
            x: 0.95047,
            y: 1.0,
            z: 1.08883,
        };
        let mut adaptation = adaption_matrix_d(
            Xyz {
                x: white[0],
                y: white[1],
                z: white[2],
            },
            d65,
        );
        let adapted_black = adaptation.mul_vector(Vector3d {
            v: black.map(f64::from),
        });
        let mut offset = Vector3d::default();
        // Fixed sRGB output policy: adapt both points, then map the declared
        // XYZ black to zero while preserving D65 white. BlackPoint is not L*.
        for (i, white) in [d65.x, d65.y, d65.z].map(f64::from).into_iter().enumerate() {
            let denominator = white - adapted_black.v[i];
            if !denominator.is_finite() || denominator <= 0.0 {
                return None;
            }
            let scale = white / denominator;
            adaptation.v[i] = adaptation.v[i].map(|value| value * scale);
            offset.v[i] = -adapted_black.v[i] * scale;
        }
        let xyz_to_rgb = Matrix3d {
            v: [
                [3.2404542, -1.5371385, -0.4985314],
                [-0.969266, 1.8760108, 0.0415560],
                [0.0556434, -0.2040259, 1.0572252],
            ],
        };
        let matrix = xyz_to_rgb.mat_mul(adaptation);
        let offset = xyz_to_rgb.mul_vector(offset);
        if matrix
            .v
            .iter()
            .flatten()
            .chain(&offset.v)
            .any(|value| !value.is_finite())
        {
            return None;
        }
        Some(Self { matrix, offset })
    }

    pub(super) fn compose(mut self, matrix: Matrix3d) -> Option<Self> {
        self.matrix = self.matrix.mat_mul(matrix);
        self.matrix
            .v
            .iter()
            .flatten()
            .all(|v| v.is_finite())
            .then_some(self)
    }

    pub(super) fn convert(&self, xyz: [f64; 3]) -> [f64; 3] {
        let linear = self.matrix.mul_vector(Vector3d { v: xyz });
        core::array::from_fn(|i| {
            let value = (linear.v[i] + self.offset.v[i]).clamp(0.0, 1.0);
            if value <= 0.0031308 {
                12.92 * value
            } else {
                1.055 * value.powf(1.0 / 2.4) - 0.055
            }
        })
    }
}

pub(super) fn numbers<const N: usize>(
    dict: &Dict<'_>,
    key: &[u8],
    default: Option<[f32; N]>,
) -> Option<[f32; N]> {
    if !dict.contains_key(key) {
        return default;
    }
    let array = dict.get::<Array<'_>>(key)?;
    // A typed iterator may stop at a non-number, including a trailing object.
    if array.raw_iter().take(N + 1).count() != N {
        return None;
    }
    let values = <[f32; N]>::try_from(array).ok()?;
    values
        .iter()
        .all(|value| value.is_finite())
        .then_some(values)
}
