use std::{io::Read, thread};

use anyhow::{Context, Result};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use smithay_client_toolkit::{
    data_device_manager::{
        DataDeviceManagerState,
        data_device::{DataDevice, DataDeviceData, DataDeviceHandler},
        data_offer::{DataOfferHandler, DragOffer},
    },
    globals::GlobalData,
};
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, delegate_dispatch, delegate_noop,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{
        wl_data_device::WlDataDevice,
        wl_data_device_manager::{DndAction, WlDataDeviceManager},
        wl_data_offer::WlDataOffer,
        wl_registry::WlRegistry,
        wl_seat::WlSeat,
        wl_surface::WlSurface,
    },
};
use winit::{event_loop::EventLoopProxy, window::Window};

use crate::UserEvent;

const URI_LIST: &str = "text/uri-list";

pub struct WaylandDrop {
    connection: Connection,
    event_queue: EventQueue<DropState>,
    state: DropState,
}

impl WaylandDrop {
    pub fn new(window: &Window, event_proxy: EventLoopProxy<UserEvent>) -> Result<Option<Self>> {
        let display = window
            .display_handle()
            .context("failed to get the native display handle")?;
        let surface = window
            .window_handle()
            .context("failed to get the native window handle")?;
        let (display, surface) = match (display.as_raw(), surface.as_raw()) {
            (RawDisplayHandle::Wayland(display), RawWindowHandle::Wayland(surface)) => {
                (display, surface)
            }
            _ => return Ok(None),
        };

        // SAFETY: winit owns both objects. `WaylandDrop` is stored inside the
        // application and is dropped before the window and event loop, so the
        // foreign display and surface remain alive for this wrapper's lifetime.
        let backend = unsafe {
            wayland_client::backend::Backend::from_foreign_display(display.display.as_ptr().cast())
        };
        let connection = Connection::from_backend(backend);
        let surface_id = unsafe {
            wayland_client::backend::ObjectId::from_ptr(
                WlSurface::interface(),
                surface.surface.as_ptr().cast(),
            )
        }
        .context("failed to import the winit Wayland surface")?;
        let target_surface = WlSurface::from_id(&connection, surface_id)
            .context("failed to wrap the winit Wayland surface")?;

        let (globals, event_queue) = registry_queue_init::<DropState>(&connection)
            .context("failed to read Wayland globals")?;
        let queue_handle = event_queue.handle();
        let manager = DataDeviceManagerState::bind(&globals, &queue_handle)
            .context("the compositor has no Wayland data-device manager")?;
        let seat = globals
            .bind::<WlSeat, _, _>(&queue_handle, 1..=9, ())
            .context("the compositor has no Wayland input seat")?;
        let data_device = manager.get_data_device(&queue_handle, &seat);

        let state = DropState {
            _manager: manager,
            _seat: seat,
            _data_device: data_device,
            target_surface,
            event_proxy,
            over_target: false,
        };
        connection
            .flush()
            .context("failed to register Wayland file-drop support")?;

        Ok(Some(Self {
            connection,
            event_queue,
            state,
        }))
    }

    pub fn dispatch_pending(&mut self) -> Result<()> {
        self.event_queue
            .dispatch_pending(&mut self.state)
            .context("failed to dispatch Wayland data-device events")?;
        self.connection
            .flush()
            .context("failed to flush Wayland data-device requests")?;
        Ok(())
    }
}

struct DropState {
    _manager: DataDeviceManagerState,
    _seat: WlSeat,
    _data_device: DataDevice,
    target_surface: WlSurface,
    event_proxy: EventLoopProxy<UserEvent>,
    over_target: bool,
}

impl Dispatch<WlRegistry, GlobalListContents> for DropState {
    fn event(
        _state: &mut Self,
        _registry: &WlRegistry,
        _event: <WlRegistry as Proxy>::Event,
        _data: &GlobalListContents,
        _connection: &Connection,
        _queue_handle: &QueueHandle<Self>,
    ) {
    }
}

delegate_noop!(DropState: ignore WlSeat);
delegate_dispatch!(DropState: [WlDataDeviceManager: GlobalData] => DataDeviceManagerState);
delegate_dispatch!(DropState: [WlDataOffer: smithay_client_toolkit::data_device_manager::data_offer::DataOfferData] => DataDeviceManagerState);
delegate_dispatch!(DropState: [WlDataDevice: DataDeviceData] => DataDeviceManagerState);

impl DataDeviceHandler for DropState {
    fn enter(
        &mut self,
        _connection: &Connection,
        _queue_handle: &QueueHandle<Self>,
        data_device: &WlDataDevice,
        _x: f64,
        _y: f64,
        surface: &WlSurface,
    ) {
        self.over_target = surface == &self.target_surface;
        if !self.over_target {
            return;
        }

        if let Some(offer) = data_device
            .data::<DataDeviceData>()
            .and_then(DataDeviceData::drag_offer)
            && offer.with_mime_types(|types| types.iter().any(|mime| mime == URI_LIST))
        {
            offer.accept_mime_type(offer.serial, Some(URI_LIST.to_owned()));
            offer.set_actions(DndAction::Copy, DndAction::Copy);
        }
    }

    fn leave(
        &mut self,
        _connection: &Connection,
        _queue_handle: &QueueHandle<Self>,
        _data_device: &WlDataDevice,
    ) {
        self.over_target = false;
    }

    fn motion(
        &mut self,
        _connection: &Connection,
        _queue_handle: &QueueHandle<Self>,
        _data_device: &WlDataDevice,
        _x: f64,
        _y: f64,
    ) {
    }

    fn selection(
        &mut self,
        _connection: &Connection,
        _queue_handle: &QueueHandle<Self>,
        _data_device: &WlDataDevice,
    ) {
    }

    fn drop_performed(
        &mut self,
        connection: &Connection,
        _queue_handle: &QueueHandle<Self>,
        data_device: &WlDataDevice,
    ) {
        if !self.over_target {
            return;
        }
        let Some(offer) = data_device
            .data::<DataDeviceData>()
            .and_then(DataDeviceData::drag_offer)
        else {
            return;
        };
        if !offer.with_mime_types(|types| types.iter().any(|mime| mime == URI_LIST)) {
            return;
        }

        offer.accept_mime_type(offer.serial, Some(URI_LIST.to_owned()));
        offer.set_actions(DndAction::Copy, DndAction::Copy);
        let pipe = match offer.receive(URI_LIST.to_owned()) {
            Ok(pipe) => pipe,
            Err(error) => {
                let _ = self
                    .event_proxy
                    .send_event(UserEvent::DropError(error.to_string()));
                return;
            }
        };
        if let Err(error) = connection.flush() {
            let _ = self
                .event_proxy
                .send_event(UserEvent::DropError(error.to_string()));
            return;
        }

        let proxy = self.event_proxy.clone();
        let connection = connection.clone();
        thread::spawn(move || receive_uri_list(pipe, offer, connection, proxy));
    }
}

impl DataOfferHandler for DropState {
    fn source_actions(
        &mut self,
        _connection: &Connection,
        _queue_handle: &QueueHandle<Self>,
        offer: &mut DragOffer,
        _actions: DndAction,
    ) {
        offer.set_actions(DndAction::Copy, DndAction::Copy);
    }

    fn selected_action(
        &mut self,
        _connection: &Connection,
        _queue_handle: &QueueHandle<Self>,
        _offer: &mut DragOffer,
        _actions: DndAction,
    ) {
    }
}

fn receive_uri_list(
    mut pipe: smithay_client_toolkit::data_device_manager::ReadPipe,
    offer: DragOffer,
    connection: Connection,
    event_proxy: EventLoopProxy<UserEvent>,
) {
    let mut data = String::new();
    let result = pipe.read_to_string(&mut data);
    if let Err(error) = result {
        let _ = event_proxy.send_event(UserEvent::DropError(error.to_string()));
    } else {
        match first_media_uri_from_uri_list(&data) {
            Ok(uri) => {
                let _ = event_proxy.send_event(UserEvent::DroppedMedia(uri));
            }
            Err(error) => {
                let _ = event_proxy.send_event(UserEvent::DropError(error));
            }
        }
    }
    offer.finish();
    offer.destroy();
    let _ = connection.flush();
}

fn first_media_uri_from_uri_list(data: &str) -> Result<url::Url, String> {
    let mut unsupported = None;
    for line in data.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Ok(uri) = url::Url::parse(line)
            && (matches!(uri.scheme(), "http" | "https")
                || (uri.scheme() == "file" && uri.to_file_path().is_ok()))
        {
            return Ok(uri);
        }
        unsupported.get_or_insert_with(|| format!("unsupported dropped URI: {line}"));
    }
    Err(unsupported.unwrap_or_else(|| "the drop contained no local files".to_owned()))
}

#[cfg(test)]
mod tests {
    use super::first_media_uri_from_uri_list;
    use std::path::PathBuf;

    #[test]
    fn parses_wayland_uri_lists_and_percent_encoding() {
        let data = "# generated by file manager\r\nfile:///tmp/a%20video.mp4\r\nfile:///tmp/second.mp4\r\n";
        let uri = first_media_uri_from_uri_list(data).unwrap();
        assert_eq!(uri.to_file_path(), Ok(PathBuf::from("/tmp/a video.mp4")));
    }

    #[test]
    fn parses_http_media_urls() {
        let uri = url::Url::parse("https://example.com/video.mp4?token=secret").unwrap();
        assert_eq!(first_media_uri_from_uri_list(uri.as_str()), Ok(uri));
    }

    #[test]
    fn rejects_unsupported_uri_schemes() {
        assert_eq!(
            first_media_uri_from_uri_list("ftp://example.com/video.mp4"),
            Err("unsupported dropped URI: ftp://example.com/video.mp4".to_owned())
        );
    }
}
