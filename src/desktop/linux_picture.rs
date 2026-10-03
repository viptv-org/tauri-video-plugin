//! Presentation-only center cropping. The viewport clips the existing native
//! surface, so a paused frame can change Fit/Fill without another decoder frame.
use gtk::prelude::*;

pub(super) struct PictureViewport {
    pub widget: gtk::Widget,
    viewport: gtk::Viewport,
    stage: gtk::Fixed,
    video: gtk::Widget,
    horizontal: gtk::Adjustment,
    vertical: gtk::Adjustment,
    width: i32,
    height: i32,
    source_width: f64,
    source_height: f64,
    fill: bool,
    applied: Option<(i32, i32, i32, i32)>,
}

impl PictureViewport {
    #[cfg(test)]
    pub(super) fn qualification_dimensions(&self) -> (i32, i32) {
        (self.video.allocated_width(), self.video.allocated_height())
    }

    pub fn new(video: gtk::Widget) -> Self {
        let horizontal = gtk::Adjustment::new(0.0, 0.0, 1.0, 1.0, 1.0, 1.0);
        let vertical = gtk::Adjustment::new(0.0, 0.0, 1.0, 1.0, 1.0, 1.0);
        let viewport = gtk::Viewport::new(Some(&horizontal), Some(&vertical));
        viewport.set_shadow_type(gtk::ShadowType::None);
        let frame = gtk::ScrolledWindow::new(Some(&horizontal), Some(&vertical));
        frame.set_policy(gtk::PolicyType::External, gtk::PolicyType::External);
        frame.set_shadow_type(gtk::ShadowType::None);
        frame.set_propagate_natural_width(false);
        frame.set_propagate_natural_height(false);
        frame.add(&viewport);
        let stage = gtk::Fixed::new();
        stage.put(&video, 0, 0);
        viewport.add(&stage);
        viewport.show();
        stage.show();
        video.show();
        Self {
            widget: frame.upcast(),
            viewport,
            stage,
            video,
            horizontal,
            vertical,
            width: 1,
            height: 1,
            source_width: 0.0,
            source_height: 0.0,
            fill: false,
            applied: None,
        }
    }

    pub fn layout(&mut self, width: f64, height: f64) {
        self.width = width.max(1.0).round() as i32;
        self.height = height.max(1.0).round() as i32;
        self.apply();
    }

    pub fn source_size(&mut self, width: f64, height: f64) {
        self.source_width = width;
        self.source_height = height;
        self.apply();
    }

    pub fn fill(&mut self, fill: bool) {
        self.fill = fill;
        self.apply();
    }

    fn apply(&mut self) {
        let (width, height) = cover_size(
            self.width,
            self.height,
            self.source_width,
            self.source_height,
            self.fill,
        );
        let geometry = (self.width, self.height, width, height);
        if self.applied == Some(geometry) {
            return;
        }
        self.applied = Some(geometry);
        self.stage.set_size_request(width, height);
        self.video.set_size_request(width, height);
        let current = self.viewport.allocation();
        self.viewport.size_allocate(&gtk::Allocation::new(
            current.x(),
            current.y(),
            self.width,
            self.height,
        ));
        self.stage
            .size_allocate(&gtk::Allocation::new(0, 0, width, height));
        self.video
            .size_allocate(&gtk::Allocation::new(0, 0, width, height));
        self.horizontal.configure(
            f64::from(width - self.width) / 2.0,
            0.0,
            f64::from(width),
            1.0,
            f64::from(self.width),
            f64::from(self.width),
        );
        self.vertical.configure(
            f64::from(height - self.height) / 2.0,
            0.0,
            f64::from(height),
            1.0,
            f64::from(self.height),
            f64::from(self.height),
        );
        self.video.queue_draw();
        self.viewport.queue_draw();
    }
}

fn cover_size(
    width: i32,
    height: i32,
    source_width: f64,
    source_height: f64,
    fill: bool,
) -> (i32, i32) {
    if !fill || source_width <= 0.0 || source_height <= 0.0 {
        return (width, height);
    }
    let scale = (f64::from(width) / source_width).max(f64::from(height) / source_height);
    (
        (source_width * scale).ceil() as i32,
        (source_height * scale).ceil() as i32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fill_preserves_aspect_and_centers_excess_inside_the_viewport() {
        assert_eq!(cover_size(1280, 680, 320.0, 240.0, false), (1280, 680));
        assert_eq!(cover_size(1280, 680, 320.0, 240.0, true), (1280, 960));
        assert_eq!(cover_size(390, 844, 1920.0, 1080.0, true), (1501, 844));
        assert_eq!(cover_size(1280, 680, 0.0, 0.0, true), (1280, 680));
    }
}

#[cfg(test)]
mod gtk_tests {
    use super::*;
    #[test]
    #[ignore = "requires a GTK display; run with xvfb-run or a real desktop"]
    fn paused_picture_mode_clips_the_existing_widget_without_growing_the_viewport() {
        gtk::init().unwrap();
        let video = gtk::DrawingArea::new();
        let mut picture = PictureViewport::new(video.upcast());
        let window = gtk::Window::new(gtk::WindowType::Toplevel);
        window.set_default_size(400, 300);
        window.add(&picture.widget);
        window.show_all();
        picture.widget.set_size_request(400, 300);
        picture
            .widget
            .size_allocate(&gtk::Allocation::new(0, 0, 400, 300));
        picture.layout(400.0, 300.0);
        picture.source_size(1920.0, 1080.0);
        picture.fill(true);
        assert_eq!(picture.viewport.allocated_width(), 400);
        assert_eq!(picture.viewport.allocated_height(), 300);
        assert_eq!(picture.video.allocated_width(), 534);
        assert_eq!(picture.video.allocated_height(), 300);
        assert_eq!(picture.horizontal.value(), 67.0);
        picture.source_size(480.0, 720.0);
        assert_eq!(picture.video.allocated_width(), 400);
        assert_eq!(picture.video.allocated_height(), 600);
        assert_eq!(picture.vertical.value(), 150.0);
        picture.fill(false);
        assert_eq!(picture.video.allocated_width(), 400);
        assert_eq!(picture.video.allocated_height(), 300);
        assert_eq!(picture.vertical.value(), 0.0);
        window.close();
    }
}
