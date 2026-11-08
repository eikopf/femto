//! Page-local raster masks. Font selection and raster settings remain fixed for the page.
use std::collections::BTreeMap;
use swash::scale::image::Image;

pub(crate) type Key = [u32; 4];

struct Entry {
    image: Option<Image>,
    used: u64,
}

pub(crate) struct GlyphCache {
    entries: BTreeMap<Key, Entry>,
    bytes: usize,
    byte_limit: usize,
    entry_limit: usize,
    clock: u64,
    uncached: Option<Image>,
    #[cfg(test)]
    pub renders: usize,
}

impl Default for GlyphCache {
    fn default() -> Self {
        Self::new(64 * 1024, 256)
    }
}

impl GlyphCache {
    pub fn new(byte_limit: usize, entry_limit: usize) -> Self {
        Self {
            entries: BTreeMap::new(),
            bytes: 0,
            byte_limit,
            entry_limit,
            clock: 0,
            uncached: None,
            #[cfg(test)]
            renders: 0,
        }
    }

    pub fn get_or_render(
        &mut self,
        key: Key,
        render: impl FnOnce() -> Option<Image>,
    ) -> Option<&Image> {
        self.uncached = None;
        self.clock += 1;
        if !self.entries.contains_key(&key) {
            #[cfg(test)]
            {
                self.renders += 1;
            }
            let image = render();
            let bytes = image.as_ref().map_or(0, |image| image.data.capacity());
            if bytes > self.byte_limit || self.entry_limit == 0 {
                self.uncached = image;
                return self.uncached.as_ref();
            }
            while self.bytes + bytes > self.byte_limit || self.entries.len() >= self.entry_limit {
                let oldest = *self.entries.iter().min_by_key(|(_, entry)| entry.used)?.0;
                let removed = self.entries.remove(&oldest)?;
                self.bytes -= removed
                    .image
                    .as_ref()
                    .map_or(0, |image| image.data.capacity());
            }
            self.bytes += bytes;
            self.entries.insert(
                key,
                Entry {
                    image,
                    used: self.clock,
                },
            );
        }
        let entry = self.entries.get_mut(&key)?;
        entry.used = self.clock;
        entry.image.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(bytes: usize) -> Option<Image> {
        Some(Image {
            data: vec![255; bytes],
            ..Default::default()
        })
    }

    #[test]
    fn eviction_obeys_buffer_capacity_and_recent_use() {
        let mut cache = GlyphCache::new(32, 4);
        cache.get_or_render([1; 4], || image(16));
        cache.get_or_render([2; 4], || image(16));
        cache.get_or_render([1; 4], || panic!("cached"));
        cache.get_or_render([3; 4], || image(16));
        assert!(cache.entries.contains_key(&[1; 4]));
        assert!(!cache.entries.contains_key(&[2; 4]));
        assert_eq!(cache.bytes, 32);
        assert_eq!(
            cache
                .get_or_render([4; 4], || image(33))
                .unwrap()
                .data
                .len(),
            33
        );
        assert!(!cache.entries.contains_key(&[4; 4]));
        assert_eq!(cache.bytes, 32);
        cache.get_or_render([1; 4], || panic!("oversize bypass preserved cache"));
        assert!(cache.uncached.is_none());
    }

    #[test]
    fn key_components_keep_font_glyph_size_and_fractional_origin_distinct() {
        let mut cache = GlyphCache::default();
        for component in 0..4 {
            let mut key = [0; 4];
            key[component] = 1;
            cache.get_or_render(key, || image(component + 1));
        }
        for component in 0..4 {
            let mut key = [0; 4];
            key[component] = 1;
            assert_eq!(
                cache
                    .get_or_render(key, || panic!("cached"))
                    .unwrap()
                    .data
                    .len(),
                component + 1
            );
        }
        assert_eq!(cache.renders, 4);
    }

    #[test]
    fn empty_glyphs_are_cached_with_bounded_entries() {
        let mut cache = GlyphCache::new(0, 2);
        for key in 0..10 {
            assert!(cache.get_or_render([key; 4], || None).is_none());
            assert!(cache.entries.len() <= 2);
        }
        assert!(
            cache
                .get_or_render([9; 4], || panic!("empty glyph cached"))
                .is_none()
        );
        assert_eq!(cache.renders, 10);
        assert_eq!(cache.bytes, 0);
    }
}
