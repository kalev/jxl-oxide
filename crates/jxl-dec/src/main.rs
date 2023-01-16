use std::path::PathBuf;

use clap::Parser;
use jxl_bitstream::{header::Headers, read_bits};
use jxl_render::RenderContext;

#[derive(Debug, Parser)]
#[command(version, about)]
struct Args {
    /// Number of threads to use, 0 to choose the value automatically
    #[arg(short, long, default_value_t)]
    threads: usize,
    /// Output file
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Input file
    input: PathBuf,
    #[arg(long, value_parser = str::parse::<CropInfo>)]
    crop: Option<CropInfo>,
}

#[derive(Debug, Default, Clone, Copy)]
struct CropInfo {
    width: u32,
    height: u32,
    left: u32,
    top: u32,
}

impl std::str::FromStr for CropInfo {
    type Err = std::num::ParseIntError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        let mut it = s.split_whitespace().map(|s| s.parse::<u32>());
        let Some(w) = it.next().transpose()? else {
            return Ok(Self {
                width: 0,
                height: 0,
                left: 0,
                top: 0,
            });
        };
        let Some(h) = it.next().transpose()? else {
            return Ok(Self {
                width: w,
                height: w,
                left: 0,
                top: 0,
            });
        };
        let Some(x) = it.next().transpose()? else {
            return Ok(Self {
                width: w,
                height: h,
                left: 0,
                top: 0,
            });
        };
        let Some(y) = it.next().transpose()? else {
            return Ok(Self {
                width: w,
                height: w,
                left: h,
                top: x,
            });
        };
        Ok(Self {
            width: w,
            height: h,
            left: x,
            top: y,
        })
    }
}

fn main() {
    let args = Args::parse();

    let file = std::fs::File::open(&args.input).expect("Failed to open file");
    let mut bitstream = jxl_bitstream::Bitstream::new(file);
    let headers = read_bits!(bitstream, Bundle(Headers)).expect("Failed to read headers");

    let crop = args.crop.and_then(|crop| {
        if crop.width == 0 && crop.height == 0 {
            None
        } else if crop.width == 0 {
            Some(CropInfo {
                width: headers.size.width,
                ..crop
            })
        } else if crop.height == 0 {
            Some(CropInfo {
                height: headers.size.height,
                ..crop
            })
        } else {
            Some(crop)
        }
    });

    let bit_depth = headers.metadata.bit_depth.bits_per_sample();
    let has_alpha = headers.metadata.alpha().is_some();

    if headers.metadata.colour_encoding.want_icc {
        let enc_size = read_bits!(bitstream, U64).unwrap();
        let mut decoder = jxl_coding::Decoder::parse(&mut bitstream, 41)
            .expect("failed to decode ICC entropy coding distribution");

        let mut encoded_icc = vec![0u8; enc_size as usize];
        let mut b1 = 0u8;
        let mut b2 = 0u8;
        decoder.begin(&mut bitstream).unwrap();
        for (idx, b) in encoded_icc.iter_mut().enumerate() {
            let sym = decoder.read_varint(&mut bitstream, get_icc_ctx(idx, b1, b2))
                .expect("Failed to read encoded ICC stream");
            if sym >= 256 {
                panic!("Decoded symbol out of range");
            }
            *b = sym as u8;

            b2 = b1;
            b1 = *b;
        }

        eprintln!("Discarding encoded ICC profile");
        drop(encoded_icc);
    }

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(args.threads)
        .build()
        .expect("failed to build thread pool");
    eprintln!("Decoding with {} threads", pool.current_num_threads());

    let mut render = RenderContext::new(&headers);
    pool.install(|| {
        if headers.metadata.have_preview {
            bitstream.zero_pad_to_byte().expect("Zero-padding failed");

            let frame = read_bits!(bitstream, Bundle(jxl_frame::Frame), &headers).expect("Failed to read frame header");

            let toc = frame.toc();
            let bookmark = toc.bookmark() + (toc.total_byte_size() * 8);
            bitstream.seek_to_bookmark(bookmark).expect("Failed to seek");
        }

        let decode_start = std::time::Instant::now();
        if let Some(crop) = &crop {
            render
                .load_cropped(&mut bitstream, Some((crop.left, crop.top, crop.width, crop.height)))
                .expect("failed to load frames");
        } else {
            render
                .load_all_frames(&mut bitstream)
                .expect("failed to load frames");
        }
        let elapsed = decode_start.elapsed();

        let elapsed_ms = elapsed.as_secs_f64() * 1000.0;
        eprintln!("Took {:.2} ms", elapsed_ms);

        if let Some(output) = args.output {
            eprintln!("Encoding samples to PNG");
            let width = render.width();
            let height = render.height();
            let output = std::fs::File::create(output).expect("failed to open output file");
            let mut encoder = png::Encoder::new(output, width, height);
            encoder.set_color(if has_alpha { png::ColorType::Rgba } else { png::ColorType::Rgb });
            encoder.set_depth(if bit_depth == 8 { png::BitDepth::Eight } else { png::BitDepth::Sixteen });
            // TODO: set colorspace
            encoder.set_srgb(match headers.metadata.colour_encoding.rendering_intent {
                jxl_bitstream::header::RenderingIntent::Perceptual => png::SrgbRenderingIntent::Perceptual,
                jxl_bitstream::header::RenderingIntent::Relative => png::SrgbRenderingIntent::RelativeColorimetric,
                jxl_bitstream::header::RenderingIntent::Saturation => png::SrgbRenderingIntent::Saturation,
                jxl_bitstream::header::RenderingIntent::Absolute => png::SrgbRenderingIntent::AbsoluteColorimetric,
            });
            let mut writer = encoder
                .write_header()
                .expect("failed to write header")
                .into_stream_writer()
                .unwrap();

            render.tmp_rgba_be_interleaved(|buf| {
                std::io::Write::write_all(&mut writer, buf).expect("failed to write image data");
                Ok(())
            }).expect("failed to write image data");
            writer.finish().expect("failed to finish writing png");
        } else {
            eprintln!("No output path specified, skipping PNG encoding");
        }
    });
}

fn get_icc_ctx(idx: usize, b1: u8, b2: u8) -> u32 {
    if idx <= 128 {
        return 0;
    }

    let p1 = match b1 {
        | b'a'..=b'z'
        | b'A'..=b'Z' => 0,
        | b'0'..=b'9'
        | b'.'
        | b',' => 1,
        | 0..=1 => 2 + b1 as u32,
        | 2..=15 => 4,
        | 241..=254 => 5,
        | 255 => 6,
        | _ => 7,
    };
    let p2 = match b2 {
        | b'a'..=b'z'
        | b'A'..=b'Z' => 0,
        | b'0'..=b'9'
        | b'.'
        | b',' => 1,
        | 0..=15 => 2,
        | 241..=255 => 3,
        | _ => 4,
    };

    1 + p1 + 8 * p2
}