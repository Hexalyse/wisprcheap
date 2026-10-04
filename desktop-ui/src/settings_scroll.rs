//! Section anchors use measured layout heights, so wrapped text and resizing stay accurate.
use std::collections::BTreeMap;

pub const GAP: f32 = 28.0;
pub const INTRO: &str = "settings-intro";

#[derive(Default)]
pub struct SettingsScroll {
    heights: BTreeMap<&'static str, f32>,
    pub offset: f32,
    viewport_height: f32,
    content_height: f32,
}

impl SettingsScroll {
    pub fn measure(&mut self, section: &'static str, height: f32) {
        self.heights.insert(section, height);
    }

    pub fn scrolled(&mut self, offset: f32, viewport_height: f32, content_height: f32) {
        self.offset = offset;
        self.viewport_height = viewport_height;
        self.content_height = content_height;
    }

    pub fn anchor(&self, section: &str, sections: &[&str]) -> Option<f32> {
        let mut offset = self.heights.get(INTRO)? + GAP;
        for name in sections {
            if *name == section {
                return Some(offset);
            }
            offset += self.heights.get(name)? + GAP;
        }
        None
    }

    pub fn active(&self, sections: &[&'static str]) -> Option<&'static str> {
        // The final heading may not reach the top when the last section is shorter than the viewport.
        if self.content_height > self.viewport_height
            && self.offset + self.viewport_height >= self.content_height - 1.0
        {
            return sections.last().copied();
        }
        sections
            .iter()
            .rev()
            .copied()
            .find(|section| {
                self.anchor(section, sections)
                    .is_some_and(|top| top <= self.offset + 8.0)
            })
            .or_else(|| sections.first().copied())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECTIONS: [&str; 3] = ["General", "Recording", "Cleanup"];

    fn layout() -> SettingsScroll {
        let mut scroll = SettingsScroll::default();
        scroll.measure(INTRO, 80.0);
        scroll.measure("General", 600.0);
        scroll.measure("Recording", 400.0);
        scroll.measure("Cleanup", 100.0);
        scroll
    }

    #[test]
    fn highlights_sections_while_scrolling_in_both_directions() {
        let mut scroll = layout();
        for (offset, expected) in [
            (0.0, "General"),
            (740.0, "Recording"),
            (1170.0, "Cleanup"),
            (800.0, "Recording"),
            (200.0, "General"),
        ] {
            scroll.scrolled(offset, 80.0, 1264.0);
            assert_eq!(scroll.active(&SECTIONS), Some(expected));
        }
    }

    #[test]
    fn reflow_moves_anchors_and_scrollbar_bottom_highlights_the_last_section() {
        let mut scroll = layout();
        assert_eq!(scroll.anchor("Recording", &SECTIONS), Some(736.0));
        scroll.measure("General", 900.0);
        assert_eq!(scroll.anchor("Recording", &SECTIONS), Some(1036.0));
        scroll.scrolled(1264.0, 300.0, 1564.0);
        assert_eq!(scroll.active(&SECTIONS), Some("Cleanup"));
    }

    #[test]
    fn waits_for_preceding_sections_to_be_measured() {
        let mut scroll = SettingsScroll::default();
        assert_eq!(scroll.anchor("Cleanup", &SECTIONS), None);
        scroll.measure(INTRO, 80.0);
        scroll.measure("General", 600.0);
        assert_eq!(scroll.anchor("Cleanup", &SECTIONS), None);
        scroll.measure("Recording", 400.0);
        assert_eq!(scroll.anchor("Cleanup", &SECTIONS), Some(1164.0));
    }
}
