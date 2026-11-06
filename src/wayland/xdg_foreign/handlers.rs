use std::collections::HashSet;

use wayland_protocols::xdg::foreign::zv2::server::{
    zxdg_exported_v2::{self, ZxdgExportedV2},
    zxdg_exporter_v2::{self, ZxdgExporterV2},
    zxdg_imported_v2::{self, ZxdgImportedV2},
    zxdg_importer_v2::{self, ZxdgImporterV2},
};
use wayland_server::{Client, DataInit, Dispatch, DisplayHandle, New, Resource, backend::ClientId};

use crate::wayland::{
    Dispatch2, GlobalData, GlobalDispatch2, compositor,
    shell::{
        is_toplevel_equivalent, is_valid_parent,
        xdg::{XDG_TOPLEVEL_ROLE, XdgShellHandler, XdgToplevelSurfaceData},
    },
};

use super::{ExportedState, XdgExportedUserData, XdgForeignHandle, XdgForeignHandler, XdgImportedUserData};

//
// Export
//

impl<D> GlobalDispatch2<ZxdgExporterV2, D> for GlobalData
where
    D: Dispatch<ZxdgExporterV2, GlobalData>,
{
    fn bind(
        &self,
        _state: &mut D,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<ZxdgExporterV2>,
        data_init: &mut DataInit<'_, D>,
    ) {
        data_init.init(resource, GlobalData);
    }
}

impl<D> Dispatch2<ZxdgExporterV2, D> for GlobalData
where
    D: Dispatch<ZxdgExportedV2, XdgExportedUserData>,
    D: XdgForeignHandler + XdgShellHandler,
{
    fn request(
        &self,
        state: &mut D,
        _client: &Client,
        resource: &ZxdgExporterV2,
        request: zxdg_exporter_v2::Request,
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            zxdg_exporter_v2::Request::ExportToplevel { id, surface } => {
                if compositor::get_role(&surface) != Some(XDG_TOPLEVEL_ROLE) {
                    resource.post_error(
                        zxdg_exporter_v2::Error::InvalidSurface,
                        "exported surface had an invalid role",
                    );
                    return;
                }

                let handle = XdgForeignHandle::new();
                let exported = data_init.init(
                    id,
                    XdgExportedUserData {
                        handle: handle.clone(),
                    },
                );
                exported.handle(handle.as_str().to_owned());

                // The export ends with its surface, even while the client keeps the xdg_exported.
                let revoked = handle.clone();
                let destruction_hook = compositor::add_destruction_hook::<D, _>(&surface, move |state, _| {
                    revoke_export(state, &revoked);
                });

                state.xdg_foreign_state().exported.insert(
                    handle,
                    ExportedState {
                        exported_surface: surface,
                        requested_child: None,
                        imported_by: HashSet::new(),
                        destruction_hook,
                        by_compositor: false,
                    },
                );
            }
            zxdg_exporter_v2::Request::Destroy => {}
            _ => {}
        }
    }
}

impl<D> Dispatch2<ZxdgExportedV2, D> for XdgExportedUserData
where
    D: XdgForeignHandler + XdgShellHandler,
{
    fn request(
        &self,
        _state: &mut D,
        _client: &Client,
        _resource: &ZxdgExportedV2,
        _request: zxdg_exported_v2::Request,
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
    }

    fn destroyed(&self, state: &mut D, _client: ClientId, _resource: &ZxdgExportedV2) {
        if let Some(exported) = state.xdg_foreign_state().exported.get(&self.handle) {
            if exported.exported_surface.is_alive() {
                compositor::remove_destruction_hook(&exported.exported_surface, &exported.destruction_hook);
            }
        }
        revoke_export(state, &self.handle);
    }
}

/// Revokes an export, when its xdg_exported or its surface is destroyed: the child it parents
/// loses that parent, and every import of the handle is told, as the protocol asks.
fn revoke_export<D>(state: &mut D, handle: &XdgForeignHandle)
where
    D: XdgForeignHandler + XdgShellHandler,
{
    invalidate_all_relationships(state, handle);
    if let Some(exported) = state.xdg_foreign_state().exported.remove(handle) {
        for imported in exported.imported_by {
            if imported.is_alive() {
                imported.destroyed();
            }
        }
    }
}

//
// Import
//

impl<D> GlobalDispatch2<ZxdgImporterV2, D> for GlobalData
where
    D: Dispatch<ZxdgImporterV2, GlobalData>,
{
    fn bind(
        &self,
        _state: &mut D,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<ZxdgImporterV2>,
        data_init: &mut DataInit<'_, D>,
    ) {
        data_init.init(resource, GlobalData);
    }
}

impl<D> Dispatch2<ZxdgImporterV2, D> for GlobalData
where
    D: Dispatch<ZxdgImportedV2, XdgImportedUserData>,
    D: XdgForeignHandler + XdgShellHandler,
{
    fn request(
        &self,
        state: &mut D,
        client: &Client,
        _resource: &ZxdgImporterV2,
        request: zxdg_importer_v2::Request,
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            zxdg_importer_v2::Request::ImportToplevel { id, handle } => {
                let exported = state
                    .xdg_foreign_state()
                    .exported
                    .iter_mut()
                    .find(|(key, _)| key.as_str() == handle.as_str());
                if let Some((_, exported)) = exported {
                    let imported = data_init.init(
                        id,
                        XdgImportedUserData {
                            handle: XdgForeignHandle(handle),
                        },
                    );
                    exported.imported_by.insert(imported);
                    return;
                }

                let Some(surface) = state
                    .foreign_toplevel(client, &handle)
                    .filter(|surface| surface.is_alive())
                else {
                    let imported = data_init.init(
                        id,
                        XdgImportedUserData {
                            handle: XdgForeignHandle(handle),
                        },
                    );
                    imported.destroyed();
                    return;
                };

                // The same name may mean another surface to another importer, so each import
                // gets an export of its own, under a key no client knows.
                let key = XdgForeignHandle::new();
                let imported = data_init.init(id, XdgImportedUserData { handle: key.clone() });
                let revoked = key.clone();
                let destruction_hook = compositor::add_destruction_hook::<D, _>(&surface, move |state, _| {
                    revoke_export(state, &revoked);
                });
                state.xdg_foreign_state().exported.insert(
                    key,
                    ExportedState {
                        exported_surface: surface,
                        requested_child: None,
                        imported_by: HashSet::from([imported]),
                        destruction_hook,
                        by_compositor: true,
                    },
                );
            }
            zxdg_importer_v2::Request::Destroy => {}
            _ => {}
        }
    }
}

impl<D> Dispatch2<ZxdgImportedV2, D> for XdgImportedUserData
where
    D: XdgForeignHandler + XdgShellHandler,
{
    fn request(
        &self,
        state: &mut D,
        _client: &Client,
        resource: &ZxdgImportedV2,
        request: zxdg_imported_v2::Request,
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            zxdg_imported_v2::Request::SetParentOf { surface: child } => {
                if let Some((_, exported_state)) = state
                    .xdg_foreign_state()
                    .exported
                    .iter_mut()
                    .find(|(key, _)| key.as_str() == self.handle.as_str())
                {
                    let parent = &exported_state.exported_surface;
                    // The compositor vouches for its own exports, which may be of another shell;
                    // such a parent has no parent of its own to make a loop with.
                    let valid = if exported_state.by_compositor && !is_toplevel_equivalent(parent) {
                        child != *parent
                    } else {
                        is_valid_parent(&child, parent)
                    };

                    let mut invalid = false;
                    let mut changed = false;
                    compositor::with_states(&child, |states| {
                        if let Some(data) = states.data_map.get::<XdgToplevelSurfaceData>() {
                            if valid {
                                let mut role = data.lock().unwrap();
                                changed = role.parent.as_ref() != Some(parent);
                                role.parent = Some(parent.clone());
                            } else {
                                invalid = true;
                            }
                        }
                    });

                    if invalid {
                        resource.post_error(
                            zxdg_imported_v2::Error::InvalidSurface,
                            "invalid parent relationship",
                        );
                        return;
                    }

                    exported_state.requested_child = Some((child.clone(), resource.clone()));

                    if changed {
                        if let Some(toplevel) = state
                            .xdg_shell_state()
                            .toplevel_surfaces()
                            .iter()
                            .find(|toplevel| *toplevel.wl_surface() == child)
                            .cloned()
                        {
                            XdgShellHandler::parent_changed(state, toplevel);
                        }
                    }
                }
            }
            zxdg_imported_v2::Request::Destroy => {}
            _ => {}
        }
    }

    fn destroyed(&self, state: &mut D, _client: ClientId, resource: &ZxdgImportedV2) {
        if let Some((_, exported_state)) = state
            .xdg_foreign_state()
            .exported
            .iter_mut()
            .find(|(key, _)| key.as_str() == self.handle.as_str())
        {
            exported_state.imported_by.remove(resource);
        }

        invalidate_relationship_for(state, &self.handle, Some(resource));

        // A compositor's export lives only as long as the import it was made for.
        let foreign_state = state.xdg_foreign_state();
        if foreign_state
            .exported
            .get(&self.handle)
            .is_some_and(|exported| exported.by_compositor)
        {
            let exported = foreign_state.exported.remove(&self.handle).unwrap();
            if exported.exported_surface.is_alive() {
                compositor::remove_destruction_hook(&exported.exported_surface, &exported.destruction_hook);
            }
        }
    }
}

fn invalidate_all_relationships<D>(state: &mut D, handle: &XdgForeignHandle)
where
    D: XdgForeignHandler + XdgShellHandler,
{
    invalidate_relationship_for(state, handle, None);
}

fn invalidate_relationship_for<D>(
    state: &mut D,
    handle: &XdgForeignHandle,
    invalidate_for: Option<&ZxdgImportedV2>,
) where
    D: XdgForeignHandler + XdgShellHandler,
{
    let Some((_, exported_state)) = state
        .xdg_foreign_state()
        .exported
        .iter_mut()
        .find(|(key, _)| key.as_str() == handle.as_str())
    else {
        return;
    };

    let Some((requested_child, requested_by)) = exported_state.requested_child.as_ref() else {
        return;
    };

    if let Some(invalidate_for) = invalidate_for {
        if invalidate_for != requested_by {
            return;
        }
    }

    let mut changed = false;
    compositor::with_states(requested_child, |states| {
        let Some(data) = states.data_map.get::<XdgToplevelSurfaceData>() else {
            return;
        };

        let data = &mut *data.lock().unwrap();
        if data.parent.as_ref() == Some(&exported_state.exported_surface) {
            data.parent = None;
            changed = true;
        }
    });

    let requested_child = requested_child.clone();
    exported_state.requested_child = None;

    if changed {
        if let Some(toplevel) = state
            .xdg_shell_state()
            .toplevel_surfaces()
            .iter()
            .find(|toplevel| *toplevel.wl_surface() == requested_child)
            .cloned()
        {
            XdgShellHandler::parent_changed(state, toplevel);
        }
    }
}
