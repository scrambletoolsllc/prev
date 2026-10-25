//! Draws one image inside a scrollable, zooms with Ctrl+scroll, pans or
//! selects by dragging, and wakes up for the next animation frame. The
//! selection is framed as a selected annotation is, and its handles
//! resize it.

use std::time::Instant;

use iced::advanced::image::{FilterMethod, Image};
use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer;
use iced::advanced::widget::{self, Tree, Widget};
use iced::advanced::{Clipboard, Shell};
use iced::keyboard::{self, Modifiers};
use iced::mouse::{self, Cursor};
use iced::widget::image::Handle;
use iced::{Background, Color, Element, Event, Length, Rectangle, Size, Theme, Vector, window};
use prev_pdf::geometry::{Point, Rect};

use super::view::Placement;
use crate::pdf::markup;
use crate::pdf::{canvas::handle_interaction, overlay};

const LINE_SCROLL_ZOOM: f32 = 1.1;
/// Zoom at or above which pixels are shown sharp instead of smoothed.
const PIXEL_ZOOM: f32 = 3.0;

/// Two corners in image pixels.
pub type Selection = (f32, f32, f32, f32);

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CanvasEvent {
    /// The visible part of the content: offset and size in logical pixels.
    ViewChanged {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    },
    ZoomBy {
        factor: f32,
        anchor: (f32, f32),
    },
    Pan {
        dx: f32,
        dy: f32,
    },
    Tick(Instant),
    /// A selection being drawn; `None` when a new drag starts.
    Selection(Option<Selection>),
}

pub struct ImageCanvas<'a, Message> {
    handle: Option<&'a Handle>,
    placement: Placement,
    /// When the next animation frame is due.
    next_frame: Option<Instant>,
    /// Dragging draws a selection instead of panning.
    selecting: bool,
    selection: Option<Selection>,
    /// Screen pixels per logical pixel.
    device_scale: f32,
    on_event: Box<dyn Fn(CanvasEvent) -> Message + 'a>,
}

impl<'a, Message> ImageCanvas<'a, Message> {
    pub fn new(
        handle: Option<&'a Handle>,
        placement: Placement,
        next_frame: Option<Instant>,
        on_event: impl Fn(CanvasEvent) -> Message + 'a,
    ) -> Self {
        Self {
            handle,
            placement,
            next_frame,
            selecting: false,
            selection: None,
            device_scale: 1.0,
            on_event: Box::new(on_event),
        }
    }

    pub fn selection(mut self, selecting: bool, selection: Option<Selection>) -> Self {
        self.selecting = selecting;
        self.selection = selection;
        self
    }

    pub fn device_scale(mut self, scale: f32) -> Self {
        self.device_scale = scale;
        self
    }

    /// The image's size in its own pixels.
    fn image_size(&self) -> (f32, f32) {
        let zoom = self.placement.zoom.max(f32::EPSILON);
        (
            self.placement.image.width / zoom,
            self.placement.image.height / zoom,
        )
    }

    /// The selection in image pixels, in order and kept on the image.
    fn selection_rect(&self) -> Option<Rect> {
        let (x0, y0, x1, y1) = self.selection?;
        let (width, height) = self.image_size();
        let on_x = |value: f32| value.clamp(0.0, width);
        let on_y = |value: f32| value.clamp(0.0, height);
        Some(Rect::new(
            on_x(x0.min(x1)),
            on_y(y0.min(y1)),
            on_x(x0.max(x1)),
            on_y(y0.max(y1)),
        ))
    }

    /// The selection's handle under `position`, a position in the window.
    fn handle_at(&self, bounds: Rectangle, position: iced::Point) -> Option<markup::Handle> {
        let rect = self.selection_rect()?;
        let (x, y) = self.to_image(bounds, position);
        let reach = markup::HANDLE_PIXELS / self.placement.zoom.max(f32::EPSILON);
        markup::hit_edge(rect, Point::new(x, y), reach)
    }

    /// A position in the window to image pixels.
    fn to_image(&self, bounds: Rectangle, position: iced::Point) -> (f32, f32) {
        let image = self.placement.image;
        let zoom = self.placement.zoom.max(f32::EPSILON);
        (
            (position.x - bounds.x - image.x) / zoom,
            (position.y - bounds.y - image.y) / zoom,
        )
    }
}

#[derive(Default)]
struct State {
    drag_from: Option<iced::Point>,
    /// Image pixel where a selection drag started.
    select_from: Option<(f32, f32)>,
    /// A handle of the selection being dragged: the selection as the drag
    /// began, the handle, and the image pixel where it began.
    resize: Option<(Rect, markup::Handle, (f32, f32))>,
    modifiers: Modifiers,
    reported: Option<(f32, f32, f32, f32)>,
}

impl<Message> Widget<Message, Theme, iced::Renderer> for ImageCanvas<'_, Message> {
    fn tag(&self) -> widget::tree::Tag {
        widget::tree::Tag::of::<State>()
    }

    fn state(&self) -> widget::tree::State {
        widget::tree::State::new(State::default())
    }

    fn size(&self) -> Size<Length> {
        let (width, height) = self.placement.content;
        Size::new(Length::Fixed(width), Length::Fixed(height))
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        _renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let (width, height) = self.placement.content;
        layout::Node::new(limits.resolve(
            Length::Fixed(width),
            Length::Fixed(height),
            Size::new(width, height),
        ))
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: Cursor,
        _renderer: &iced::Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_mut::<State>();
        let bounds = layout.bounds();

        let view = (
            viewport.x - bounds.x,
            viewport.y - bounds.y,
            viewport.width,
            viewport.height,
        );
        if state.reported != Some(view) && view.2 > 0.0 && view.3 > 0.0 {
            state.reported = Some(view);
            let (x, y, width, height) = view;
            shell.publish((self.on_event)(CanvasEvent::ViewChanged {
                x,
                y,
                width,
                height,
            }));
        }

        if let Some(due) = self.next_frame {
            if let Event::Window(window::Event::RedrawRequested(now)) = event
                && *now >= due
            {
                shell.publish((self.on_event)(CanvasEvent::Tick(*now)));
            } else {
                shell.request_redraw_at(window::RedrawRequest::At(due));
            }
        }

        match event {
            Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) => {
                state.modifiers = *modifiers
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                if let Some(position) = cursor.position_over(*viewport) {
                    if self.selecting
                        && let Some(handle) = self.handle_at(bounds, position)
                        && let Some(rect) = self.selection_rect()
                    {
                        state.resize = Some((rect, handle, self.to_image(bounds, position)));
                    } else if self.selecting {
                        state.select_from = Some(self.to_image(bounds, position));
                        shell.publish((self.on_event)(CanvasEvent::Selection(None)));
                    } else {
                        state.drag_from = Some(position);
                    }
                    shell.capture_event();
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { position }) => {
                if let Some((
                    original,
                    markup::Handle::Edge {
                        x: side_x,
                        y: side_y,
                    },
                    from,
                )) = state.resize
                {
                    let (x, y) = self.to_image(bounds, cursor.position().unwrap_or(*position));
                    let (width, height) = self.image_size();
                    let (dx, dy) = (x.clamp(0.0, width) - from.0, y.clamp(0.0, height) - from.1);
                    let keep_ratio = state.modifiers.shift();
                    let rect = markup::resized(original, side_x, side_y, dx, dy, keep_ratio);
                    shell.publish((self.on_event)(CanvasEvent::Selection(Some((
                        rect.x0, rect.y0, rect.x1, rect.y1,
                    )))));
                } else if let Some((x0, y0)) = state.select_from {
                    // The event has window coordinates; the cursor given to
                    // this widget is moved by the scroll offset.
                    let (x1, y1) = self.to_image(bounds, cursor.position().unwrap_or(*position));
                    shell.publish((self.on_event)(CanvasEvent::Selection(Some((
                        x0, y0, x1, y1,
                    )))));
                } else if let Some(from) = state.drag_from {
                    // The content moves under the cursor, so pan against it.
                    let (dx, dy) = (from.x - position.x, from.y - position.y);
                    state.drag_from = Some(*position);
                    if dx != 0.0 || dy != 0.0 {
                        shell.publish((self.on_event)(CanvasEvent::Pan { dx, dy }));
                    }
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                state.drag_from = None;
                state.select_from = None;
                state.resize = None;
            }
            Event::Mouse(mouse::Event::WheelScrolled { delta }) if state.modifiers.command() => {
                let Some(position) = cursor.position_over(*viewport) else {
                    return;
                };
                let lines = crate::pdf::canvas::wheel_steps(delta);
                if lines != 0.0 {
                    let anchor = (position.x - viewport.x, position.y - viewport.y);
                    let factor = LINE_SCROLL_ZOOM.powf(lines);
                    shell.publish((self.on_event)(CanvasEvent::ZoomBy { factor, anchor }));
                }
                shell.capture_event();
            }
            _ => {}
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: Cursor,
        viewport: &Rectangle,
        _renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        let state = tree.state.downcast_ref::<State>();
        if let Some((_, handle, _)) = state.resize {
            return handle_interaction(handle);
        }
        if self.selecting
            && let Some(position) = cursor.position_over(*viewport)
        {
            return self
                .handle_at(layout.bounds(), position)
                .map_or(mouse::Interaction::Crosshair, handle_interaction);
        }
        let (content_width, content_height) = self.placement.content;
        let scrolls =
            content_width > viewport.width + 0.5 || content_height > viewport.height + 0.5;
        if state.drag_from.is_some() || cursor.is_over(*viewport) && scrolls {
            mouse::Interaction::Idle
        } else {
            mouse::Interaction::None
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        _theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: Cursor,
        viewport: &Rectangle,
    ) {
        use iced::advanced::Renderer as _;
        use iced::advanced::image::Renderer as _;
        let Some(handle) = self.handle else { return };
        let bounds = layout.bounds();
        let image = self.placement.image;
        let rect = Rectangle::new(
            iced::Point::new(bounds.x + image.x, bounds.y + image.y),
            Size::new(image.width, image.height),
        );
        let filter = if self.placement.zoom >= PIXEL_ZOOM {
            FilterMethod::Nearest
        } else {
            FilterMethod::Linear
        };
        renderer.draw_image(
            Image::new(handle.clone()).filter_method(filter),
            rect,
            *viewport,
        );

        let Some(selection) = self.selection_rect() else {
            return;
        };
        let zoom = self.placement.zoom;
        let (left, right) = (rect.x + selection.x0 * zoom, rect.x + selection.x1 * zoom);
        let (top, bottom) = (rect.y + selection.y0 * zoom, rect.y + selection.y1 * zoom);
        let area = |x: f32, y: f32, width: f32, height: f32| renderer::Quad {
            bounds: Rectangle::new(
                iced::Point::new(x, y),
                Size::new(width.max(0.0), height.max(0.0)),
            ),
            ..renderer::Quad::default()
        };
        // Quads draw before images within a layer, so the overlay gets its own.
        renderer.with_layer(*viewport, |renderer| {
            let shade = Background::Color(Color::from_rgba(0.0, 0.0, 0.0, 0.5));
            renderer.fill_quad(area(rect.x, rect.y, rect.width, top - rect.y), shade);
            renderer.fill_quad(
                area(rect.x, bottom, rect.width, rect.y + rect.height - bottom),
                shade,
            );
            renderer.fill_quad(area(rect.x, top, left - rect.x, bottom - top), shade);
            renderer.fill_quad(
                area(right, top, rect.x + rect.width - right, bottom - top),
                shade,
            );
        });
        // The box on top of the shade, with its handles once drawn.
        use iced::advanced::graphics::geometry::Renderer as _;
        let drawing = tree.state.downcast_ref::<State>().select_from.is_some();
        let mut frame = iced::widget::canvas::Frame::new(renderer, viewport.size());
        overlay::selection_rect(
            &mut frame,
            iced::Point::new(left - viewport.x, top - viewport.y),
            Size::new(right - left, bottom - top),
            !drawing,
            self.device_scale,
        );
        let geometry = frame.into_geometry();
        renderer.with_layer(*viewport, |renderer| {
            renderer.with_translation(Vector::new(viewport.x, viewport.y), |renderer| {
                renderer.draw_geometry(geometry);
            });
        });
    }
}

impl<'a, Message: 'a> From<ImageCanvas<'a, Message>> for Element<'a, Message> {
    fn from(canvas: ImageCanvas<'a, Message>) -> Self {
        Element::new(canvas)
    }
}
