use crate::{
    backend::renderer::{
        ImportAll, Renderer,
        element::{
            AsRenderElements, Kind,
            surface::{WaylandSurfaceRenderElement, render_elements_from_surface_tree},
        },
    },
    desktop::{PopupManager, Window, WindowSurface, WindowSurfaceType, space::SpaceElement},
    output::Output,
    utils::{Logical, Physical, Point, Rectangle, Scale},
    wayland::seat::WaylandFocus,
};

use super::{WindowOutputUserData, output_update};

impl SpaceElement for Window {
    fn geometry(&self) -> Rectangle<i32, Logical> {
        self.geometry()
    }

    fn bbox(&self) -> Rectangle<i32, Logical> {
        self.bbox_with_popups()
    }

    fn is_in_input_region(&self, point: &Point<f64, Logical>) -> bool {
        self.surface_under(*point, WindowSurfaceType::ALL).is_some()
    }

    fn z_index(&self) -> u8 {
        self.0.z_index.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn set_activate(&self, activated: bool) {
        self.set_activated(activated);
    }

    #[profiling::function]
    fn output_enter(&self, output: &Output, overlap: Rectangle<i32, Logical>) {
        self.user_data().insert_if_missing(WindowOutputUserData::default);
        {
            let mut state = self
                .user_data()
                .get::<WindowOutputUserData>()
                .unwrap()
                .borrow_mut();
            state.output_overlap.insert(output.downgrade(), overlap);
            state.output_overlap.retain(|weak, _| weak.is_alive());
        }
        self.refresh()
    }

    #[profiling::function]
    fn output_leave(&self, output: &Output) {
        if let Some(state) = self.user_data().get::<WindowOutputUserData>() {
            state.borrow_mut().output_overlap.retain(|weak, _| weak != output);
        }

        if let Some(surface) = self.wl_surface() {
            output_update(output, None, &surface);
            for (popup, _) in PopupManager::popups_for_surface(&surface) {
                output_update(output, None, popup.wl_surface());
            }
        }
    }

    #[profiling::function]
    fn refresh(&self) {
        self.user_data().insert_if_missing(WindowOutputUserData::default);
        let state = self.user_data().get::<WindowOutputUserData>().unwrap().borrow();

        if let Some(surface) = self.wl_surface() {
            // The stored overlap is relative to the element's bounding box top-left
            // (computed by Space::refresh as overlap.loc -= bbox.loc). However,
            // output_update traverses the surface tree starting at (0, 0) for the
            // root wl_surface. For windows with client-side decorations (CSD), the
            // bounding box extends beyond the root surface origin (e.g., title bar
            // at negative Y). We must adjust the overlap to be relative to the root
            // surface origin so that CSD subsurfaces pass the overlap check.
            let bbox_loc = self.bbox().loc;

            for (weak, overlap) in state.output_overlap.iter() {
                if let Some(output) = weak.upgrade() {
                    let mut surface_overlap = *overlap;
                    surface_overlap.loc += bbox_loc;
                    output_update(&output, Some(surface_overlap), &surface);
                    for (popup, location) in PopupManager::popups_for_surface(&surface) {
                        let mut popup_overlap = surface_overlap;
                        popup_overlap.loc -= location;
                        output_update(&output, Some(popup_overlap), popup.wl_surface());
                    }
                }
            }
        }
    }
}

impl<R> AsRenderElements<R> for Window
where
    R: Renderer + ImportAll,
    R::TextureId: Clone + 'static,
{
    type RenderElement = WaylandSurfaceRenderElement<R>;

    #[profiling::function]
    fn render_elements<C: From<WaylandSurfaceRenderElement<R>>>(
        &self,
        renderer: &mut R,
        location: Point<i32, Physical>,
        scale: Scale<f64>,
        alpha: f32,
    ) -> Vec<C> {
        match self.underlying_surface() {
            WindowSurface::Wayland(s) => {
                let mut render_elements: Vec<C> = Vec::new();
                let surface = s.wl_surface();
                let popup_render_elements =
                    PopupManager::popups_for_surface(surface).flat_map(|(popup, popup_offset)| {
                        let offset = (self.geometry().loc + popup_offset - popup.geometry().loc)
                            .to_physical_precise_round(scale);

                        render_elements_from_surface_tree(
                            renderer,
                            popup.wl_surface(),
                            location + offset,
                            scale,
                            alpha,
                            Kind::Unspecified,
                        )
                    });

                render_elements.extend(popup_render_elements);

                render_elements.extend(render_elements_from_surface_tree(
                    renderer,
                    surface,
                    location,
                    scale,
                    alpha,
                    Kind::Unspecified,
                ));

                render_elements
            }
            #[cfg(feature = "xwayland")]
            WindowSurface::X11(s) => AsRenderElements::render_elements(s, renderer, location, scale, alpha),
        }
    }
}
