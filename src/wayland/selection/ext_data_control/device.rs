use std::cell::RefCell;

use wayland_protocols::ext::data_control::v1::server::ext_data_control_device_v1::{
    self, ExtDataControlDeviceV1,
};
use wayland_server::Resource;
use wayland_server::protocol::wl_seat::WlSeat;
use wayland_server::{Client, DisplayHandle};

use crate::input::Seat;
use crate::wayland::Dispatch2;
use crate::wayland::pending::{Effect, Pending};
use crate::wayland::selection::device::SelectionDevice;
use crate::wayland::selection::offer::OfferReplySource;
use crate::wayland::selection::seat_data::SeatData;
use crate::wayland::selection::source::SelectionSourceProvider;
use crate::wayland::selection::{SelectionSource, SelectionTarget};
use wayland_protocols::ext::data_control::v1::server::ext_data_control_source_v1::ExtDataControlSourceV1;

use super::DataControlHandler;

#[doc(hidden)]
#[derive(Debug)]
pub struct ExtDataControlDeviceUserData {
    pub(crate) primary: bool,
    pub(crate) wl_seat: WlSeat,
}

impl<D> Dispatch2<ExtDataControlDeviceV1, D> for ExtDataControlDeviceUserData
where
    D: DataControlHandler,
    D: 'static,
{
    fn request(
        &self,
        handler: &mut D,
        client: &Client,
        resource: &ExtDataControlDeviceV1,
        request: <ExtDataControlDeviceV1 as wayland_server::Resource>::Request,
        dh: &DisplayHandle,
        _: &mut wayland_server::DataInit<'_, D>,
    ) {
        let seat = match Seat::<D>::from_resource(&self.wl_seat) {
            Some(seat) => seat,
            None => return,
        };

        match request {
            ext_data_control_device_v1::Request::SetSelection { source, .. } => {
                // Each source can only be used once.
                if let Some(source) = source.as_ref() {
                    if handler
                        .data_control_state()
                        .used_sources
                        .insert(source.clone(), self.wl_seat.clone())
                        .is_some()
                    {
                        resource.post_error(
                            ext_data_control_device_v1::Error::UsedSource,
                            "selection source can be used only once.",
                        );
                        return;
                    }
                }

                let pending = selection_request::<D>(&self.wl_seat, dh, SelectionTarget::Clipboard, source);
                handler.data_control_request(client, pending);
            }
            ext_data_control_device_v1::Request::SetPrimarySelection { source, .. } => {
                // When the primary selection is disabled, we should simply ignore the requests.
                if !self.primary {
                    return;
                }

                // Each source can only be used once.
                if let Some(source) = source.as_ref() {
                    if handler
                        .data_control_state()
                        .used_sources
                        .insert(source.clone(), self.wl_seat.clone())
                        .is_some()
                    {
                        resource.post_error(
                            ext_data_control_device_v1::Error::UsedSource,
                            "selection source can be used only once.",
                        );
                        return;
                    }
                }

                let pending = selection_request::<D>(&self.wl_seat, dh, SelectionTarget::Primary, source);
                handler.data_control_request(client, pending);
            }
            ext_data_control_device_v1::Request::Destroy => seat
                .user_data()
                .get::<RefCell<SeatData<D::SelectionUserData>>>()
                .unwrap()
                .borrow_mut()
                .retain_devices(|ndd| match ndd {
                    SelectionDevice::ExtDataControl(ndd) => ndd != resource,
                    _ => true,
                }),

            _ => unreachable!(),
        }
    }

    fn destroyed(
        &self,
        _state: &mut D,
        _client: wayland_server::backend::ClientId,
        resource: &ExtDataControlDeviceV1,
    ) {
        if let Some(seat) = Seat::<D>::from_resource(&self.wl_seat) {
            if let Some(seat_data) = seat.user_data().get::<RefCell<SeatData<D::SelectionUserData>>>() {
                seat_data.borrow_mut().retain_devices(|ndd| match ndd {
                    SelectionDevice::ExtDataControl(ndd) => ndd != resource,
                    _ => true,
                });
            }
        }
    }
}

/// Setting `target` to `source`, as a request the compositor may keep or refuse.
fn selection_request<D>(
    wl_seat: &WlSeat,
    dh: &DisplayHandle,
    target: SelectionTarget,
    source: Option<ExtDataControlSourceV1>,
) -> Pending<D>
where
    D: DataControlHandler + 'static,
{
    let (wl_seat, dh, cancelled) = (wl_seat.clone(), dh.clone(), source.clone());
    Pending::new(
        Effect::Change,
        move |handler: &mut D| {
            // A source destroyed while the request was kept has nothing to offer.
            if source.as_ref().is_some_and(|source| !source.is_alive()) {
                return;
            }
            let Some(seat) = Seat::<D>::from_resource(&wl_seat) else {
                return;
            };
            seat.user_data()
                .insert_if_missing(|| RefCell::new(SeatData::<D::SelectionUserData>::new()));
            let source = source.map(SelectionSourceProvider::ExtDataControl);
            handler.new_selection(
                target,
                source.clone().map(|provider| SelectionSource { provider }),
                seat.clone(),
            );
            let mut seat_data = seat
                .user_data()
                .get::<RefCell<SeatData<D::SelectionUserData>>>()
                .unwrap()
                .borrow_mut();
            let source = source.map(OfferReplySource::Client);
            match target {
                SelectionTarget::Clipboard => seat_data.set_clipboard_selection::<D>(&dh, source),
                SelectionTarget::Primary => seat_data.set_primary_selection::<D>(&dh, source),
            }
        },
        move |_| {
            if let Some(source) = cancelled {
                source.cancelled();
            }
        },
    )
}
