use femto_picco_board::{Error, PLANE_BYTES, orientation::Orientation};
const ALL: [Orientation; 4] = [
    Orientation::Portrait,
    Orientation::Clockwise,
    Orientation::Inverted,
    Orientation::Counterclockwise,
];

#[test]
fn all_view_pixels_map_bijectively_and_match_native_bitplanes() {
    for orientation in ALL {
        let (width, height) = orientation.dimensions();
        let mut seen = vec![false; 480 * 800];
        let pixels: Vec<_> = (0..height)
            .flat_map(|y| (0..width).map(move |x| (((x / 7 + y / 11) % 4) * 85) as u8))
            .collect();
        let mono: Vec<_> = pixels
            .iter()
            .map(|p| if *p < 128 { 0 } else { 255 })
            .collect();
        let mut high = vec![0; PLANE_BYTES];
        let mut low = high.clone();
        let mut plane = high.clone();
        orientation.grayscale(&pixels, &mut high, &mut low).unwrap();
        orientation.monochrome(&mono, &mut plane).unwrap();
        for y in 0..height {
            for x in 0..width {
                let portrait = orientation.to_portrait(x, y).unwrap();
                assert_eq!(
                    orientation.from_portrait(portrait.0, portrait.1),
                    Some((x, y))
                );
                let (nx, ny) = match orientation {
                    Orientation::Portrait => (y, 479 - x),
                    Orientation::Clockwise => (799 - x, 479 - y),
                    Orientation::Inverted => (799 - y, x),
                    Orientation::Counterclockwise => (x, y),
                };
                let native = ny * 800 + nx;
                assert!(!seen[native]);
                seen[native] = true;
                let bit = 0x80 >> (native % 8);
                let value = u8::from(high[native / 8] & bit != 0) * 2
                    + u8::from(low[native / 8] & bit != 0);
                assert_eq!(value * 85, pixels[y * width + x]);
                assert_eq!(plane[native / 8] & bit != 0, mono[y * width + x] != 0);
            }
        }
        assert!(seen.into_iter().all(|p| p));
        assert_eq!(orientation.to_portrait(width, 0), None);
        assert_eq!(orientation.to_portrait(0, height), None);
        assert_eq!(orientation.to_portrait(usize::MAX, usize::MAX), None);
        assert_eq!(orientation.from_portrait(480, 0), None);
        assert_eq!(orientation.from_portrait(0, 800), None);
    }
}
#[test]
fn invalid_frames_preserve_every_output_in_every_orientation() {
    for orientation in ALL {
        let mut pixels = vec![255; 480 * 800];
        let mut high = vec![0xa5; PLANE_BYTES];
        let mut low = vec![0x5a; PLANE_BYTES];
        pixels[100] = 127;
        assert_eq!(
            orientation.monochrome(&pixels, &mut high),
            Err(Error::FrameEncoding)
        );
        assert_eq!(
            orientation.grayscale(&pixels, &mut high, &mut low),
            Err(Error::FrameEncoding)
        );
        assert!(high.iter().all(|p| *p == 0xa5));
        assert!(low.iter().all(|p| *p == 0x5a));
        pixels[100] = 255;
        assert_eq!(
            orientation.grayscale(&pixels, &mut high, &mut low[..1]),
            Err(Error::FrameSize)
        );
        assert_eq!(
            orientation.monochrome(&pixels[..10], &mut high),
            Err(Error::FrameSize)
        );
        assert!(high.iter().all(|p| *p == 0xa5));
        assert!(low.iter().all(|p| *p == 0x5a));
    }
}
