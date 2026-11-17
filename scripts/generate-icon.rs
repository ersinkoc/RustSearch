//! Reproduce the native vector mark as a multi-resolution Windows ICO, without external tools.
//! Run: rustc scripts/generate-icon.rs -O -o .temp_files/generate-icon.exe
//!      .temp_files/generate-icon.exe
use std::io::Write;

fn segment(x: f64, y: f64, a: (f64, f64), b: (f64, f64), width: f64) -> bool {
    let dx = b.0 - a.0;
    let dy = b.1 - a.1;
    let t = (((x - a.0) * dx + (y - a.1) * dy) / (dx * dx + dy * dy)).clamp(0., 1.);
    (x - a.0 - t * dx).hypot(y - a.1 - t * dy) <= width / 2.
}
fn polygon(x: f64, y: f64, points: &[(f64, f64)]) -> bool {
    let mut inside = false;
    for (a, b) in points
        .iter()
        .zip(points.iter().cycle().skip(1))
        .take(points.len())
    {
        if (a.1 > y) != (b.1 > y) && x < (b.0 - a.0) * (y - a.1) / (b.1 - a.1) + a.0 {
            inside = !inside;
        }
    }
    inside
}
fn sample(x: f64, y: f64) -> [u8; 4] {
    let dark = [16, 39, 36, 255];
    let teal = [85, 219, 200, 255];
    let qx = (x - 128.).abs() - 64.;
    let qy = (y - 128.).abs() - 64.;
    if qx.max(0.).hypot(qy.max(0.)) + qx.max(qy).min(0.) > 56. {
        return [0; 4];
    }
    let mut color = dark;
    let folder = [
        (58., 76.),
        (107., 76.),
        (124., 93.),
        (198., 93.),
        (198., 177.),
        (58., 177.),
    ];
    if folder
        .iter()
        .zip(folder.iter().cycle().skip(1))
        .take(folder.len())
        .any(|(&a, &b)| segment(x, y, a, b, 12.))
    {
        color = teal;
    }
    let distance = (x - 119.).hypot(y - 131.);
    if distance < 23.5 {
        color = dark;
    }
    if (distance - 30.).abs() <= 6.5 || segment(x, y, (141., 153.), (177., 189.), 15.) {
        color = teal;
    }
    if polygon(
        x,
        y,
        &[
            (187., 44.),
            (167., 74.),
            (186., 74.),
            (178., 100.),
            (209., 64.),
            (187., 64.),
            (199., 44.),
        ],
    ) {
        color = [232, 189, 120, 255];
    }
    color
}
fn main() -> std::io::Result<()> {
    let sizes = [16u32, 24, 32, 48, 64, 128, 256];
    let mut images = Vec::new();
    for n in sizes {
        let mask_stride = n.div_ceil(32) * 4;
        let mut bytes = Vec::new();
        for v in [40u32, n, n * 2] {
            bytes.extend(v.to_le_bytes());
        }
        bytes.extend(1u16.to_le_bytes());
        bytes.extend(32u16.to_le_bytes());
        for v in [0u32, n * n * 4, 0, 0, 0, 0] {
            bytes.extend(v.to_le_bytes());
        }
        for y in (0..n).rev() {
            for x in 0..n {
                let mut sum = [0u32; 4];
                for sy in 0..4 {
                    for sx in 0..4 {
                        let c = sample(
                            (x as f64 + (sx as f64 + 0.5) / 4.) * 256. / n as f64,
                            (y as f64 + (sy as f64 + 0.5) / 4.) * 256. / n as f64,
                        );
                        if c[3] != 0 {
                            for i in 0..3 {
                                sum[i] += c[i] as u32;
                            }
                            sum[3] += 1;
                        }
                    }
                }
                let count = sum[3].max(1);
                bytes.extend([
                    (sum[2] / count) as u8,
                    (sum[1] / count) as u8,
                    (sum[0] / count) as u8,
                    (sum[3] * 255 / 16) as u8,
                ]);
            }
        }
        bytes.resize(bytes.len() + (mask_stride * n) as usize, 0);
        images.push(bytes);
    }
    let mut file = std::fs::File::create("crates/desktop/assets/rustsearch.ico")?;
    file.write_all(&[0, 0, 1, 0, sizes.len() as u8, 0])?;
    let mut offset = 6 + 16 * sizes.len() as u32;
    for (n, image) in sizes.iter().zip(&images) {
        file.write_all(&[*n as u8, *n as u8, 0, 0, 1, 0, 32, 0])?;
        file.write_all(&(image.len() as u32).to_le_bytes())?;
        file.write_all(&offset.to_le_bytes())?;
        offset += image.len() as u32;
    }
    for image in images {
        file.write_all(&image)?;
    }
    Ok(())
}
