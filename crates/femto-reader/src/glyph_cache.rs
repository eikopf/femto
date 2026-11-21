//! Bounded raster masks. Entries are valid for one font set and raster configuration,
//! identified by the owner's tag.
//!
//! Masks share one buffer and the index is one table, so a device allocator
//! that reserves scarce internal memory for small allocations places the
//! cache in external memory.
use std::collections::HashMap;
use swash::{scale::image::Image, zeno::Placement};

pub(crate) type Key = [u32; 4];

pub(crate) struct Mask<'a> {
    pub placement: Placement,
    pub data: &'a [u8],
}

pub(crate) struct GlyphCache {
    /// `None` records a glyph without a mask.
    entries: HashMap<Key, Option<(Placement, std::ops::Range<usize>)>>,
    masks: Vec<u8>,
    byte_limit: usize,
    entry_limit: usize,
    uncached: Option<Image>,
    tag: u64,
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
            entries: HashMap::new(),
            masks: Vec::new(),
            byte_limit,
            entry_limit,
            uncached: None,
            tag: 0,
            #[cfg(test)]
            renders: 0,
        }
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.masks.clear();
    }

    /// Discards every mask when the fonts or raster settings differ from the
    /// ones the retained masks were rendered with.
    pub fn retag(&mut self, tag: u64) {
        if self.tag != tag {
            self.clear();
            self.tag = tag;
        }
    }

    /// A full cache is emptied rather than evicting single masks: the working
    /// set of a book is far below the limits, and a shared buffer has no holes.
    pub fn get_or_render(
        &mut self,
        key: Key,
        render: impl FnOnce() -> Option<Image>,
    ) -> Option<Mask<'_>> {
        self.uncached = None;
        if !self.entries.contains_key(&key) {
            #[cfg(test)]
            {
                self.renders += 1;
            }
            let image = render();
            let bytes = image.as_ref().map_or(0, |image| image.data.len());
            if bytes > self.byte_limit || self.entry_limit == 0 {
                self.uncached = image;
                return self.uncached.as_ref().map(|image| Mask {
                    placement: image.placement,
                    data: &image.data,
                });
            }
            if self.masks.len() + bytes > self.byte_limit || self.entries.len() >= self.entry_limit
            {
                self.clear();
            }
            if self.entries.capacity() == 0 {
                self.entries.reserve(self.entry_limit);
                self.masks.reserve_exact(self.byte_limit);
            }
            let entry = image.map(|image| {
                let start = self.masks.len();
                self.masks.extend_from_slice(&image.data);
                (image.placement, start..self.masks.len())
            });
            self.entries.insert(key, entry);
        }
        let (placement, range) = self.entries.get(&key)?.clone()?;
        Some(Mask {
            placement,
            data: &self.masks[range],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(bytes: usize) -> Option<Image> {
        Some(Image {
            data: vec![bytes as u8; bytes],
            ..Default::default()
        })
    }

    fn len(cache: &mut GlyphCache, key: u32, render: impl FnOnce() -> Option<Image>) -> usize {
        let mask = cache.get_or_render([key; 4], render).unwrap();
        assert!(
            mask.data
                .iter()
                .all(|&byte| byte as usize == mask.data.len())
        );
        mask.data.len()
    }

    #[test]
    fn limits_empty_the_cache_and_oversize_masks_bypass_it() {
        let mut cache = GlyphCache::new(32, 4);
        assert_eq!(len(&mut cache, 1, || image(16)), 16);
        assert_eq!(len(&mut cache, 2, || image(15)), 15);
        assert_eq!(len(&mut cache, 1, || panic!("cached")), 16);
        assert_eq!(len(&mut cache, 4, || image(33)), 33);
        assert_eq!(
            len(&mut cache, 2, || panic!("oversize bypass preserved cache")),
            15
        );
        assert!(cache.uncached.is_none());
        // The byte limit empties the cache before the new mask is stored.
        assert_eq!(len(&mut cache, 3, || image(2)), 2);
        assert_eq!(cache.entries.len(), 1);
        assert_eq!(cache.masks.len(), 2);
        for key in 10..13 {
            len(&mut cache, key, || image(1));
        }
        assert_eq!(cache.entries.len(), 4);
        len(&mut cache, 13, || image(1));
        assert_eq!(cache.entries.len(), 1);
        assert!(cache.masks.capacity() <= 64);
    }

    #[test]
    fn retagging_discards_masks_only_on_change() {
        let mut cache = GlyphCache::default();
        cache.retag(7);
        len(&mut cache, 1, || image(8));
        cache.retag(7);
        len(&mut cache, 1, || panic!("cached"));
        cache.retag(8);
        assert!(cache.masks.is_empty());
        len(&mut cache, 1, || image(8));
        assert_eq!(cache.renders, 2);
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
        assert!(cache.masks.is_empty());
    }
}
