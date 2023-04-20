use jxl_grid::SimpleGrid;

pub fn perform_inverse_ycbcr(fb_cbycr: [&mut SimpleGrid<f32>; 3]) {
    let [cb, y, cr] = fb_cbycr;
    let cb = cb.buf_mut();
    let y = y.buf_mut();
    let cr = cr.buf_mut();

    for ((r, g), b) in cb.iter_mut().zip(y).zip(cr) {
        let cb = *r;
        let y = *g + 0.5;
        let cr = *b;

        *r = y + 1.402 * cr;
        *g = y - 0.344016 * cb - 0.714136 * cr;
        *b = y + 1.772 * cb;
    }
}

pub fn ycbcr_upsample(grids_cbycr: [&mut SimpleGrid<f32>; 3], jpeg_upsampling: [u32; 3]) {
    fn interpolate(left: f32, center: f32, right: f32) -> (f32, f32) {
        (0.25 * left + 0.75 * center, 0.75 * center + 0.25 * right)
    }

    let shifts_cbycr = [0, 1, 2].map(|idx| {
        jxl_modular::ChannelShift::from_jpeg_upsampling(jpeg_upsampling, idx)
    });

    for (buf, shift) in grids_cbycr.into_iter().zip(shifts_cbycr) {
        let width = buf.width();
        let height = buf.height();
        let buf = buf.buf_mut();

        let h_upsampled = shift.hshift() == 0;
        let v_upsampled = shift.vshift() == 0;

        if !h_upsampled {
            let orig_width = width;
            let width = (width + 1) / 2;
            let height = if v_upsampled { height } else { (height + 1) / 2 };

            for y in 0..height {
                let idx_base = y * orig_width;
                let mut prev_sample = buf[idx_base + width - 1];
                for x in (0..width).rev() {
                    let curr_sample = buf[idx_base + x];
                    let left_x = x.saturating_sub(1);

                    let (me, next) = interpolate(
                        prev_sample,
                        curr_sample,
                        buf[idx_base + left_x],
                    );

                    buf[idx_base + x * 2] = me;
                    if x * 2 + 1 < orig_width {
                        buf[idx_base + x * 2 + 1] = next;
                    }

                    prev_sample = curr_sample;
                }
            }
        }

        // image is horizontally upsampled here
        if !v_upsampled {
            let orig_height = height;
            let height = (height + 1) / 2;

            let mut prev_row = buf[(height - 1) * width..][..width].to_vec();
            for y in (0..height).rev() {
                let idx_base = y * width;
                let bottom_base = if y == height - 1 { idx_base } else { idx_base + width };
                for x in 0..width {
                    let curr_sample = buf[idx_base + x];

                    let (me, next) = interpolate(
                        prev_row[x],
                        curr_sample,
                        buf[bottom_base + x],
                    );
                    buf[idx_base * 2 + x] = me;
                    if y * 2 + 1 < orig_height {
                        buf[idx_base * 2 + width + x] = next;
                    }

                    prev_row[x] = curr_sample;
                }
            }
        }
    }
}
