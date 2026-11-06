//! Implementation `xdg_foreign` protocol
//!
//! ```rs
//! # extern crate wayland_server;
//! #
//! use wayland_server::{protocol::wl_surface::WlSurface, DisplayHandle};
//! use smithay::{
//!     delegate_xdg_foreign,
//!     wayland::xdg_foreign::{XdgForeignHandler, XdgForeignState}
//! };
//!
//! pub struct State {
//!     xdg_foreign_state: XdgForeignState,
//! }
//!
//! impl XdgForeignHandler for State {
//!     fn xdg_foreignHandler_state(&mut self) -> &mut XdgForeignState {
//!         &mut self.xdg_foreign_state
//!     }
//! }
//!
//! // Delegate xdg foreign handling for State to XdgForeignState.
//! delegate_xdg_foreign!(State);
//!
//! # let mut display = wayland_server::Display::<State>::new().unwrap();
//! # let display_handle = display.handle();
//! let state = State {
//!     xdg_foreign_state: XdgForeignState::new::<State>(&display_handle),
//! };
//! ```

use std::{
    collections::{HashMap, HashSet},
    ops::Deref,
};

use rand::distr::{Alphanumeric, SampleString};
use wayland_protocols::xdg::foreign::zv2::server::{
    zxdg_exporter_v2::ZxdgExporterV2, zxdg_imported_v2::ZxdgImportedV2, zxdg_importer_v2::ZxdgImporterV2,
};
use wayland_server::{
    Client, DisplayHandle, GlobalDispatch, backend::GlobalId, protocol::wl_surface::WlSurface,
};

use crate::{utils::HookId, wayland::GlobalData};

mod handlers;

/// A trait implemented to be notified of activation requests using the xdg foreign protocol.
pub trait XdgForeignHandler: 'static {
    /// Returns the xdg foreign state.
    fn xdg_foreign_state(&mut self) -> &mut XdgForeignState;

    /// The toplevel a handle names that no client exported, such as an X11 window the
    /// compositor names by its id. `importer` is the client importing it.
    ///
    /// The default knows none, so their imports are destroyed at once.
    fn foreign_toplevel(&mut self, importer: &Client, handle: &str) -> ForeignToplevel {
        let _ = (importer, handle);
        ForeignToplevel::Unknown
    }
}

/// What [`XdgForeignHandler::foreign_toplevel`] makes of a handle no client exported.
#[derive(Debug)]
pub enum ForeignToplevel {
    /// Nothing: the import is destroyed at once, as for any unknown handle.
    Unknown,
    /// A toplevel that can't be a parent now, such as a window not mapped yet: the import
    /// lives on but parents nothing, so the importer carries on as if it had no parent.
    Unparented,
    /// The import parents the importer's toplevels to this surface, which need not be an
    /// `xdg_toplevel`, and is told `destroyed` when the surface is.
    Surface(WlSurface),
}

/// The handle contains the unique handle of exported surface.
/// It may be shared with any client, which then can use it to import the surface by calling xdg_importer.import_toplevel.
/// A handle may be used to import the surface multiple times.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct XdgForeignHandle(String);

impl XdgForeignHandle {
    fn new() -> Self {
        Self(Alphanumeric.sample_string(&mut rand::rng(), 32))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

impl Deref for XdgForeignHandle {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

/// User data of xdg_exported
#[derive(Debug)]
pub struct XdgExportedUserData {
    handle: XdgForeignHandle,
}

/// User data of xdg_imported
#[derive(Debug)]
pub struct XdgImportedUserData {
    handle: XdgForeignHandle,
}

#[derive(Debug)]
struct ExportedState {
    exported_surface: WlSurface,
    requested_child: Option<(WlSurface, ZxdgImportedV2)>,
    imported_by: HashSet<ZxdgImportedV2>,
    destruction_hook: HookId,
    /// Made for one import by [`XdgForeignHandler::foreign_toplevel`], and ended with it.
    by_compositor: bool,
}

/// Tracks the list of exported surfaces
#[derive(Debug)]
pub struct XdgForeignState {
    exported: HashMap<XdgForeignHandle, ExportedState>,
    exporter: GlobalId,
    importer: GlobalId,
}

impl XdgForeignState {
    /// Creates a new xdg activation global.
    ///
    /// In order to use this abstraction, your `D` type needs to implement [`XdgForeignHandler`].
    pub fn new<D>(display: &DisplayHandle) -> Self
    where
        D: XdgForeignHandler,
        D: GlobalDispatch<ZxdgExporterV2, GlobalData>,
        D: GlobalDispatch<ZxdgImporterV2, GlobalData>,
    {
        let exporter = display.create_global::<D, ZxdgExporterV2, _>(1, GlobalData);
        let importer = display.create_global::<D, ZxdgImporterV2, _>(1, GlobalData);

        Self {
            exported: HashMap::new(),
            exporter,
            importer,
        }
    }

    /// Returns the xdg_exporter global.
    pub fn exporter_global(&self) -> GlobalId {
        self.exporter.clone()
    }

    /// Returns the xdg_importer global.
    pub fn importer_global(&self) -> GlobalId {
        self.importer.clone()
    }
}

#[cfg(test)]
mod tests {
    use std::{os::unix::net::UnixStream, sync::Arc};

    use wayland_client::{
        Connection, Dispatch, EventQueue, QueueHandle, delegate_noop,
        globals::{GlobalListContents, registry_queue_init},
        protocol::{
            wl_compositor::WlCompositor, wl_registry::WlRegistry, wl_surface::WlSurface as ClientSurface,
        },
    };
    use wayland_protocols::xdg::{
        foreign::zv2::client::{
            zxdg_exported_v2::{self, ZxdgExportedV2},
            zxdg_exporter_v2::ZxdgExporterV2 as ClientExporter,
            zxdg_imported_v2::{self, ZxdgImportedV2 as ClientImported},
            zxdg_importer_v2::ZxdgImporterV2 as ClientImporter,
        },
        shell::client::{xdg_surface::XdgSurface, xdg_toplevel::XdgToplevel, xdg_wm_base::XdgWmBase},
    };
    use wayland_server::{
        Client, Display,
        backend::{ClientData, ClientId, DisconnectReason},
        protocol::wl_seat::WlSeat,
    };

    use super::*;
    use crate::{
        utils::Serial,
        wayland::{
            compositor::{CompositorClientState, CompositorHandler, CompositorState},
            shell::xdg::{PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState},
        },
    };

    struct Server {
        compositor: CompositorState,
        xdg_shell: XdgShellState,
        foreign: XdgForeignState,
        /// Every surface made, in order.
        surfaces: Vec<WlSurface>,
        /// What [`XdgForeignHandler::foreign_toplevel`] resolves, by importer and name.
        named: HashMap<(ClientId, String), WlSurface>,
        /// Names it resolves to [`ForeignToplevel::Unparented`].
        unparented: HashSet<String>,
    }

    #[derive(Default)]
    struct ClientState {
        compositor: CompositorClientState,
    }

    impl ClientData for ClientState {
        fn initialized(&self, _client_id: ClientId) {}
        fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
    }

    impl CompositorHandler for Server {
        fn compositor_state(&mut self) -> &mut CompositorState {
            &mut self.compositor
        }
        fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
            &client.get_data::<ClientState>().unwrap().compositor
        }
        fn commit(&mut self, _surface: &WlSurface) {}
        fn new_surface(&mut self, surface: &WlSurface) {
            self.surfaces.push(surface.clone());
        }
    }

    impl XdgShellHandler for Server {
        fn xdg_shell_state(&mut self) -> &mut XdgShellState {
            &mut self.xdg_shell
        }
        fn new_toplevel(&mut self, _surface: ToplevelSurface) {}
        fn new_popup(&mut self, _surface: PopupSurface, _positioner: PositionerState) {}
        fn grab(&mut self, _surface: PopupSurface, _seat: WlSeat, _serial: Serial) {}
        fn reposition_request(&mut self, _surface: PopupSurface, _positioner: PositionerState, _token: u32) {}
    }

    impl XdgForeignHandler for Server {
        fn xdg_foreign_state(&mut self) -> &mut XdgForeignState {
            &mut self.foreign
        }

        fn foreign_toplevel(&mut self, importer: &Client, handle: &str) -> ForeignToplevel {
            match self.named.get(&(importer.id(), handle.to_owned())) {
                Some(surface) => ForeignToplevel::Surface(surface.clone()),
                None if self.unparented.contains(handle) => ForeignToplevel::Unparented,
                None => ForeignToplevel::Unknown,
            }
        }
    }

    crate::delegate_dispatch2!(Server);

    #[derive(Default)]
    struct App {
        handle: Option<String>,
        destroyed: usize,
    }

    impl Dispatch<WlRegistry, GlobalListContents> for App {
        fn event(
            _: &mut Self,
            _: &WlRegistry,
            _: <WlRegistry as wayland_client::Proxy>::Event,
            _: &GlobalListContents,
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }

    impl Dispatch<ZxdgExportedV2, ()> for App {
        fn event(
            app: &mut Self,
            _: &ZxdgExportedV2,
            event: zxdg_exported_v2::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            if let zxdg_exported_v2::Event::Handle { handle } = event {
                app.handle = Some(handle);
            }
        }
    }

    impl Dispatch<ClientImported, ()> for App {
        fn event(
            app: &mut Self,
            _: &ClientImported,
            event: zxdg_imported_v2::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            if let zxdg_imported_v2::Event::Destroyed = event {
                app.destroyed += 1;
            }
        }
    }

    delegate_noop!(App: WlCompositor);
    delegate_noop!(App: ClientExporter);
    delegate_noop!(App: ClientImporter);
    delegate_noop!(App: ignore ClientSurface);
    delegate_noop!(App: ignore XdgWmBase);
    delegate_noop!(App: ignore XdgSurface);
    delegate_noop!(App: ignore XdgToplevel);

    struct Peer {
        id: ClientId,
        queue: EventQueue<App>,
        app: App,
        compositor: WlCompositor,
        wm_base: XdgWmBase,
        exporter: ClientExporter,
        importer: ClientImporter,
    }

    struct Window {
        surface: ClientSurface,
        xdg_surface: XdgSurface,
        toplevel: XdgToplevel,
    }

    impl Peer {
        fn window(&self) -> Window {
            let qh = self.queue.handle();
            let surface = self.compositor.create_surface(&qh, ());
            let xdg_surface = self.wm_base.get_xdg_surface(&surface, &qh, ());
            let toplevel = xdg_surface.get_toplevel(&qh, ());
            Window {
                surface,
                xdg_surface,
                toplevel,
            }
        }
    }

    struct Harness {
        display: Display<Server>,
        server: Server,
    }

    impl Harness {
        fn new() -> Self {
            let display = Display::<Server>::new().unwrap();
            let dh = display.handle();
            let server = Server {
                compositor: CompositorState::new::<Server>(&dh),
                xdg_shell: XdgShellState::new::<Server>(&dh),
                foreign: XdgForeignState::new::<Server>(&dh),
                surfaces: Vec::new(),
                named: HashMap::new(),
                unparented: HashSet::new(),
            };
            Self { display, server }
        }

        fn connect(&mut self) -> Peer {
            let (server_end, client_end) = UnixStream::pair().unwrap();
            let id = self
                .display
                .handle()
                .insert_client(server_end, Arc::new(ClientState::default()))
                .unwrap()
                .id();
            let conn = Connection::from_socket(client_end).unwrap();
            let (globals, queue) = registry_queue_init_pumped(self, &conn);
            let qh = queue.handle();
            let mut peer = Peer {
                id,
                compositor: globals.bind(&qh, 1..=5, ()).unwrap(),
                wm_base: globals.bind(&qh, 1..=1, ()).unwrap(),
                exporter: globals.bind(&qh, 1..=1, ()).unwrap(),
                importer: globals.bind(&qh, 1..=1, ()).unwrap(),
                app: App::default(),
                queue,
            };
            self.pump(&mut [(&mut peer.queue, &mut peer.app)]);
            peer
        }

        /// Passes requests and events back and forth until both sides are idle.
        fn pump(&mut self, peers: &mut [(&mut EventQueue<App>, &mut App)]) {
            for _ in 0..8 {
                for (queue, _) in peers.iter_mut() {
                    queue.flush().unwrap();
                }
                self.display.dispatch_clients(&mut self.server).unwrap();
                self.display.flush_clients().unwrap();
                for (queue, app) in peers.iter_mut() {
                    if let Some(guard) = queue.prepare_read() {
                        let _ = guard.read();
                    }
                    queue.dispatch_pending(app).unwrap();
                }
            }
        }

        fn pump_peers(&mut self, a: &mut Peer, b: &mut Peer) {
            self.pump(&mut [(&mut a.queue, &mut a.app), (&mut b.queue, &mut b.app)]);
        }

        /// The parent each server-side toplevel has, in creation order.
        fn parents(&self) -> Vec<Option<WlSurface>> {
            self.server
                .xdg_shell
                .toplevel_surfaces()
                .iter()
                .map(|toplevel| toplevel.parent())
                .collect()
        }
    }

    /// `registry_queue_init` blocks on a roundtrip, so the server runs while a thread waits on it.
    fn registry_queue_init_pumped(
        harness: &mut Harness,
        conn: &Connection,
    ) -> (wayland_client::globals::GlobalList, EventQueue<App>) {
        std::thread::scope(|scope| {
            let client = scope.spawn(|| registry_queue_init::<App>(conn).unwrap());
            while !client.is_finished() {
                harness.display.dispatch_clients(&mut harness.server).unwrap();
                harness.display.flush_clients().unwrap();
                std::thread::yield_now();
            }
            client.join().unwrap()
        })
    }

    /// A dialog of another client, parented through an import of `parent`'s export.
    fn dialog_of(
        harness: &mut Harness,
        parent: &mut Peer,
        child: &mut Peer,
    ) -> (Window, Window, ZxdgExportedV2) {
        let parent_window = parent.window();
        let exported = parent
            .exporter
            .export_toplevel(&parent_window.surface, &parent.queue.handle(), ());
        harness.pump_peers(parent, child);
        let handle = parent.app.handle.clone().expect("the export has a handle");

        let child_window = child.window();
        let imported = child.importer.import_toplevel(handle, &child.queue.handle(), ());
        imported.set_parent_of(&child_window.surface);
        harness.pump_peers(parent, child);
        let parents = harness.parents();
        assert_eq!(parents.len(), 2);
        assert!(parents[1].is_some(), "the dialog has the exported parent");
        (parent_window, child_window, exported)
    }

    #[test]
    fn destroying_the_exported_surface_tells_its_importers() {
        let mut harness = Harness::new();
        let mut parent = harness.connect();
        let mut child = harness.connect();
        let (parent_window, _child_window, exported) = dialog_of(&mut harness, &mut parent, &mut child);

        // The parent closes its window but keeps the xdg_exported object.
        parent_window.toplevel.destroy();
        parent_window.xdg_surface.destroy();
        parent_window.surface.destroy();
        harness.pump_peers(&mut parent, &mut child);
        assert_eq!(child.app.destroyed, 1, "the import learns its parent is gone");
        assert_eq!(harness.parents(), vec![None], "the dialog has no parent left");

        // Destroying the export afterwards sends nothing more.
        exported.destroy();
        harness.pump_peers(&mut parent, &mut child);
        assert_eq!(child.app.destroyed, 1);
    }

    #[test]
    fn destroying_the_export_first_tells_importers_once() {
        let mut harness = Harness::new();
        let mut parent = harness.connect();
        let mut child = harness.connect();
        let (parent_window, _child_window, exported) = dialog_of(&mut harness, &mut parent, &mut child);

        exported.destroy();
        harness.pump_peers(&mut parent, &mut child);
        assert_eq!(child.app.destroyed, 1);
        assert_eq!(harness.parents()[1], None);

        parent_window.toplevel.destroy();
        parent_window.xdg_surface.destroy();
        parent_window.surface.destroy();
        harness.pump_peers(&mut parent, &mut child);
        assert_eq!(
            child.app.destroyed, 1,
            "the surface's end does not revoke the export again"
        );
    }

    #[test]
    fn an_exporter_that_disconnects_tells_its_importers() {
        let mut harness = Harness::new();
        let mut parent = harness.connect();
        let mut child = harness.connect();
        let (parent_window, child_window, exported) = dialog_of(&mut harness, &mut parent, &mut child);

        // The parent app exits without destroying anything.
        drop((parent_window, exported, parent));
        harness.pump(&mut [(&mut child.queue, &mut child.app)]);
        assert_eq!(child.app.destroyed, 1);
        assert_eq!(harness.parents(), vec![None]);
        drop(child_window);
    }
    /// A surface of `owner` with no role, as an X11 window's is, that the compositor names
    /// `name` for `importer`.
    fn named_surface(harness: &mut Harness, owner: &mut Peer, importer: &Peer, name: &str) -> ClientSurface {
        let surface = owner.compositor.create_surface(&owner.queue.handle(), ());
        surface.commit();
        harness.pump(&mut [(&mut owner.queue, &mut owner.app)]);
        let server_surface = harness.server.surfaces.last().unwrap().clone();
        harness
            .server
            .named
            .insert((importer.id.clone(), name.to_owned()), server_surface);
        surface
    }

    #[test]
    fn a_name_the_compositor_resolves_parents_a_dialog_and_ends_with_its_surface() {
        let mut harness = Harness::new();
        let mut owner = harness.connect();
        let mut child = harness.connect();
        let surface = named_surface(&mut harness, &mut owner, &child, "x11:1");
        let named = harness.server.surfaces.last().unwrap().clone();

        let dialog = child.window();
        let imported = child
            .importer
            .import_toplevel("x11:1".into(), &child.queue.handle(), ());
        imported.set_parent_of(&dialog.surface);
        harness.pump_peers(&mut owner, &mut child);
        assert_eq!(child.app.destroyed, 0, "the import is live");
        assert_eq!(
            harness.parents(),
            vec![Some(named)],
            "the dialog's parent is the named surface"
        );

        surface.destroy();
        harness.pump_peers(&mut owner, &mut child);
        assert_eq!(child.app.destroyed, 1, "the import learns its parent is gone");
        assert_eq!(harness.parents(), vec![None]);
        assert!(harness.server.foreign.exported.is_empty());
    }

    #[test]
    fn a_name_resolves_only_for_the_importer_it_was_resolved_for() {
        let mut harness = Harness::new();
        let mut owner = harness.connect();
        let child = harness.connect();
        let mut other = harness.connect();
        let _surface = named_surface(&mut harness, &mut owner, &child, "x11:1");

        let _imported = other
            .importer
            .import_toplevel("x11:1".into(), &other.queue.handle(), ());
        harness.pump_peers(&mut owner, &mut other);
        assert_eq!(
            other.app.destroyed, 1,
            "another client's import of the name is unknown"
        );
        assert!(harness.server.foreign.exported.is_empty());
    }

    #[test]
    fn a_compositor_export_ends_with_its_import() {
        let mut harness = Harness::new();
        let mut owner = harness.connect();
        let mut child = harness.connect();
        let surface = named_surface(&mut harness, &mut owner, &child, "x11:1");

        let first = child
            .importer
            .import_toplevel("x11:1".into(), &child.queue.handle(), ());
        let second = child
            .importer
            .import_toplevel("x11:1".into(), &child.queue.handle(), ());
        harness.pump_peers(&mut owner, &mut child);
        assert_eq!(
            harness.server.foreign.exported.len(),
            2,
            "each import has an export"
        );

        first.destroy();
        harness.pump_peers(&mut owner, &mut child);
        assert_eq!(harness.server.foreign.exported.len(), 1);
        second.destroy();
        harness.pump_peers(&mut owner, &mut child);
        assert!(harness.server.foreign.exported.is_empty());

        surface.destroy();
        harness.pump_peers(&mut owner, &mut child);
        assert_eq!(child.app.destroyed, 0, "no import is left to tell");
    }

    #[test]
    fn a_name_that_cannot_parent_yet_keeps_its_import_and_parents_nothing() {
        let mut harness = Harness::new();
        let mut owner = harness.connect();
        let mut child = harness.connect();
        harness.server.unparented.insert("x11:2".into());

        let dialog = child.window();
        let imported = child
            .importer
            .import_toplevel("x11:2".into(), &child.queue.handle(), ());
        imported.set_parent_of(&dialog.surface);
        harness.pump_peers(&mut owner, &mut child);
        assert_eq!(child.app.destroyed, 0, "the import is not destroyed");
        assert_eq!(harness.parents(), vec![None], "the dialog has no parent");
        assert!(harness.server.foreign.exported.is_empty());

        imported.destroy();
        harness.pump_peers(&mut owner, &mut child);
        assert_eq!(child.app.destroyed, 0);
    }
}
