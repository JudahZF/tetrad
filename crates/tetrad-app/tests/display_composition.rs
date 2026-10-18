use tetrad_sys::display::{Channel, FramebufferInfo, PixelFormat};
use tetrad_ui::UiState;

#[test]
fn owned_ui_frame_composes_with_explicit_framebuffer_layout_without_device_io() {
    let frame = UiState::new().render();
    let channel = |offset| Channel {
        offset,
        length: 8,
        msb_right: 0,
    };
    // Explicit synthetic xRGB8888 LE surface with a border and line padding.
    // This layout is not claimed to be the stock QC framebuffer mode.
    let stride = (frame.width() + 4) * 4;
    let info = FramebufferInfo {
        width: frame.width(),
        height: frame.height(),
        virtual_width: frame.width() + 2,
        virtual_height: frame.height() + 2,
        xoffset: 1,
        yoffset: 1,
        line_length: stride,
        memory_length: stride * (frame.height() + 2),
        framebuffer_type: 0,
        visual: 2,
        grayscale: 0,
        nonstandard: 0,
        vmode: 0,
        rotate: 0,
        format: PixelFormat {
            bits_per_pixel: 32,
            red: channel(16),
            green: channel(8),
            blue: channel(0),
            alpha: Channel::default(),
        },
    };
    let layout = info.layout().unwrap();
    let plan = layout
        .plan_rgb8(frame.width(), frame.height(), frame.rgb())
        .unwrap();
    let mut backing = vec![0xa5; info.memory_length as usize];
    plan.apply_to_slice(&mut backing).unwrap();
    assert!(backing[..stride as usize].iter().all(|&b| b == 0xa5));
    for y in 0..frame.height() as usize {
        let line = (y + 1) * stride as usize;
        assert_eq!(&backing[line..line + 4], &[0xa5; 4]);
        for x in 0..frame.width() as usize {
            let source = (y * frame.width() as usize + x) * 3;
            let dest = line + (x + 1) * 4;
            assert_eq!(
                &backing[dest..dest + 4],
                &[
                    frame.rgb()[source + 2],
                    frame.rgb()[source + 1],
                    frame.rgb()[source],
                    0
                ]
            );
        }
        assert!(
            backing[line + (frame.width() as usize + 1) * 4..line + stride as usize]
                .iter()
                .all(|&b| b == 0xa5)
        );
    }
    let mut short = vec![0xa5; 10];
    assert!(plan.apply_to_slice(&mut short).is_err());
    assert_eq!(short, [0xa5; 10]);
}
