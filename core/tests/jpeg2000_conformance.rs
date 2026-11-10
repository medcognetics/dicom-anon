//! ITU-T T.803 | ISO/IEC 15444-4 class 1 conformance of the JPEG 2000 decoder (#47).
//!
//! Each class 1 codestream of profiles 0 and 1 is decoded with the OpenJPEG call that the
//! burned-in text check uses (strict mode), and each component is compared with its
//! reference within the peak error and MSE limits of T.803 Tables C.6 and C.7, as
//! OpenJPEG's own test suite applies them. The data is not in this repository; run
//! `scripts/fetch-jpeg2000-conformance.sh DIR` and set `DICOM_ANON_J2K_CONFORMANCE=DIR`.
//! Without the variable the test does nothing.

use std::path::Path;

struct Case {
    file: &'static str,
    reference: &'static str,
    /// Resolution reduction factor.
    reduce: u32,
    /// Peak error and MSE limits per component; the length is the component count.
    limits: &'static [(i64, f64)],
}

const CASES: [Case; 23] = [
    Case {
        file: "p0_01.j2k",
        reference: "c1p0_01",
        reduce: 0,
        limits: &[(0, 0.0)],
    },
    Case {
        file: "p0_02.j2k",
        reference: "c1p0_02",
        reduce: 0,
        limits: &[(0, 0.0)],
    },
    Case {
        file: "p0_03.j2k",
        reference: "c1p0_03",
        reduce: 0,
        limits: &[(0, 0.0)],
    },
    Case {
        file: "p0_04.j2k",
        reference: "c1p0_04",
        reduce: 0,
        limits: &[(5, 0.776), (4, 0.626), (6, 1.070)],
    },
    Case {
        file: "p0_05.j2k",
        reference: "c1p0_05",
        reduce: 0,
        limits: &[(2, 0.302), (2, 0.307), (2, 0.269), (0, 0.0)],
    },
    Case {
        file: "p0_06.j2k",
        reference: "c1p0_06",
        reduce: 0,
        limits: &[(635, 11287.0), (403, 6124.0), (378, 3968.0), (0, 0.0)],
    },
    Case {
        file: "p0_07.j2k",
        reference: "c1p0_07",
        reduce: 0,
        limits: &[(0, 0.0), (0, 0.0), (0, 0.0)],
    },
    Case {
        file: "p0_08.j2k",
        reference: "c1p0_08",
        reduce: 1,
        limits: &[(0, 0.0), (0, 0.0), (0, 0.0)],
    },
    Case {
        file: "p0_09.j2k",
        reference: "c1p0_09",
        reduce: 0,
        limits: &[(0, 0.0)],
    },
    Case {
        file: "p0_10.j2k",
        reference: "c1p0_10",
        reduce: 0,
        limits: &[(0, 0.0), (0, 0.0), (0, 0.0)],
    },
    Case {
        file: "p0_11.j2k",
        reference: "c1p0_11",
        reduce: 0,
        limits: &[(0, 0.0)],
    },
    Case {
        file: "p0_12.j2k",
        reference: "c1p0_12",
        reduce: 0,
        limits: &[(0, 0.0)],
    },
    Case {
        file: "p0_13.j2k",
        reference: "c1p0_13",
        reduce: 0,
        limits: &[(0, 0.0), (0, 0.0), (0, 0.0), (0, 0.0)],
    },
    Case {
        file: "p0_14.j2k",
        reference: "c1p0_14",
        reduce: 0,
        limits: &[(0, 0.0), (0, 0.0), (0, 0.0)],
    },
    Case {
        file: "p0_15.j2k",
        reference: "c1p0_15",
        reduce: 0,
        limits: &[(0, 0.0)],
    },
    Case {
        file: "p0_16.j2k",
        reference: "c1p0_16",
        reduce: 0,
        limits: &[(0, 0.0)],
    },
    Case {
        file: "p1_01.j2k",
        reference: "c1p1_01",
        reduce: 0,
        limits: &[(0, 0.0)],
    },
    Case {
        file: "p1_02.j2k",
        reference: "c1p1_02",
        reduce: 0,
        limits: &[(5, 0.765), (4, 0.616), (6, 1.051)],
    },
    Case {
        file: "p1_03.j2k",
        reference: "c1p1_03",
        reduce: 0,
        limits: &[(2, 0.3), (2, 0.210), (1, 0.200), (0, 0.0)],
    },
    Case {
        file: "p1_04.j2k",
        reference: "c1p1_04",
        reduce: 0,
        limits: &[(624, 3080.0)],
    },
    Case {
        file: "p1_05.j2k",
        reference: "c1p1_05",
        reduce: 0,
        limits: &[(40, 8.458), (40, 9.816), (40, 10.154)],
    },
    Case {
        file: "p1_06.j2k",
        reference: "c1p1_06",
        reduce: 0,
        limits: &[(2, 0.6), (2, 0.6), (2, 0.6)],
    },
    Case {
        file: "p1_07.j2k",
        reference: "c1p1_07",
        reduce: 0,
        limits: &[(0, 0.0), (0, 0.0)],
    },
];

/// A PGX component: `PG <ML|LM> <+|-><depth> <width> <height>\n` and raw samples.
fn read_pgx(path: &Path) -> (usize, usize, Vec<i64>) {
    let bytes = std::fs::read(path).unwrap();
    let end = bytes.iter().position(|&byte| byte == b'\n').unwrap();
    let header = std::str::from_utf8(&bytes[..end]).unwrap();
    let fields: Vec<&str> = header.split_whitespace().collect();
    // The sign may be written apart from the depth, as `+ 12`.
    let (big_endian, rest) = (fields[1] == "ML", fields[2..].join(" "));
    let rest = rest.replace("+ ", "+").replace("- ", "-");
    let parts: Vec<&str> = rest.split_whitespace().collect();
    let signed = parts[0].starts_with('-');
    let depth: usize = parts[0].trim_start_matches(['+', '-']).parse().unwrap();
    let (width, height): (usize, usize) = (parts[1].parse().unwrap(), parts[2].parse().unwrap());
    let size = if depth <= 8 {
        1
    } else if depth <= 16 {
        2
    } else {
        4
    };
    let data = &bytes[end + 1..];
    assert_eq!(data.len(), width * height * size, "{path:?}");
    let samples = data
        .chunks_exact(size)
        .map(|chunk| {
            let mut value: u64 = 0;
            for (index, &byte) in chunk.iter().enumerate() {
                let shift = if big_endian {
                    (size - 1 - index) * 8
                } else {
                    index * 8
                };
                value |= u64::from(byte) << shift;
            }
            let bits = (size * 8) as u32;
            if signed && value >> (bits - 1) == 1 {
                value as i64 - (1_i64 << bits)
            } else {
                value as i64
            }
        })
        .collect();
    (width, height, samples)
}

#[test]
fn class_1_codestreams_decode_within_the_t803_limits() {
    let Some(directory) = std::env::var_os("DICOM_ANON_J2K_CONFORMANCE") else {
        eprintln!("DICOM_ANON_J2K_CONFORMANCE is not set; skipping the conformance test");
        return;
    };
    let directory = Path::new(&directory);
    let mut failures = Vec::new();
    for case in &CASES {
        let codestream = std::fs::read(directory.join(case.file)).unwrap();
        let parameters = openjpeg::DecodeParameters::new()
            .strict(true)
            .reduce(case.reduce);
        let image = match openjpeg::Image::from_bytes_with(&codestream, parameters) {
            Ok(image) => image,
            Err(error) => {
                failures.push(format!("{}: decode failed: {error}", case.file));
                continue;
            }
        };
        for (index, &(peak_limit, mse_limit)) in case.limits.iter().enumerate() {
            let (width, height, expected) =
                read_pgx(&directory.join(format!("{}_{index}.pgx", case.reference)));
            let Some(component) = image.components().get(index) else {
                failures.push(format!("{} component {index}: missing", case.file));
                continue;
            };
            if (component.width() as usize, component.height() as usize) != (width, height) {
                failures.push(format!("{} component {index}: size differs", case.file));
                continue;
            }
            let (mut peak, mut squared) = (0_i64, 0_f64);
            for (&decoded, &reference) in component.data().iter().zip(&expected) {
                let difference = (i64::from(decoded) - reference).abs();
                peak = peak.max(difference);
                squared += (difference * difference) as f64;
            }
            let mse = squared / expected.len() as f64;
            if peak > peak_limit || mse > mse_limit {
                failures.push(format!(
                    "{} component {index}: peak {peak} (limit {peak_limit}), MSE {mse:.3} (limit {mse_limit})",
                    case.file
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}
