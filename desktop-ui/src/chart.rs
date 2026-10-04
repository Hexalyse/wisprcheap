use iced::widget::canvas::{self, Frame, Geometry};
use iced::{Color, Point, Rectangle, Renderer, Size, Theme, mouse};
use wisprcheap::companion::StatRow;

pub struct ActivityChart<'a>(pub &'a [StatRow]);

impl<Message> canvas::Program<Message> for ActivityChart<'_> {
    type State = ();

    fn draw(
        &self,
        _: &(),
        renderer: &Renderer,
        _: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let width = bounds.width - 30.0;
        let height = bounds.height - 42.0;
        let max = self.0.iter().map(|r| r.words).max().unwrap_or(1).max(1) as f32;
        let step = width / self.0.len().max(1) as f32;
        frame.fill_rectangle(
            Point::new(15.0, height),
            Size::new(width, 1.0),
            Color::from_rgb8(60, 72, 88),
        );
        for (i, row) in self.0.iter().enumerate() {
            let h = height * row.words as f32 / max;
            frame.fill_rectangle(
                Point::new(15.0 + i as f32 * step, height - h),
                Size::new((step - 4.0).max(1.0), h),
                Color::from_rgb8(89, 205, 183),
            );
            if i == 0 || i == self.0.len() - 1 || (self.0.len() > 12 && i == self.0.len() / 2) {
                frame.fill_text(canvas::Text {
                    content: row.label.get(5..).unwrap_or(&row.label).to_string(),
                    position: Point::new(15.0 + i as f32 * step, height + 10.0),
                    color: Color::from_rgb8(160, 173, 190),
                    size: 12.0.into(),
                    ..Default::default()
                });
            }
        }
        vec![frame.into_geometry()]
    }
}
