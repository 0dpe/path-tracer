pub mod render; // pub just for the benchmark
mod utils;

use render::state::State;
use utils::expect_universal::ExpectUniversal;

// not supported because testing inlcudes saving images to disk
#[cfg(all(feature = "testing", target_arch = "wasm32"))]
compile_error!("feature `testing` is not supported on wasm32");

// if target_arch is wasm32, apply wasm_bindgen(start)
// this cfg_attr done because wasm_bindgen dependency isn't in scope for non-wasm
// wasm_bindgen(start) marks the first function that gets executed when this code, converted to wasm, is loaded into the browser
#[cfg_attr(target_arch = "wasm32", wasm_bindgen::prelude::wasm_bindgen(start))]
pub fn run() {
    #[cfg(not(target_arch = "wasm32"))]
    env_logger::init();

    #[cfg(target_arch = "wasm32")]
    {
        console_error_panic_hook::set_once();
        console_log::init_with_level(log::Level::Info).expect_universal("Console log failed"); // the hierarchy of log is: Error Warn Info Debug Trace
    }

    // running on the web, this will show since console_log was initialized with level Info
    // running natively, this won't show in the terminal by default, but in ../.cargo/config.toml, Info level is set so this will show
    // if needed, to explicitly let the terminal show up to Info level:
    // On MacOS or Linux: RUST_LOG=info cargo run
    // In PowerShell: $env:RUST_LOG="info"; cargo run
    log::info!("Logging enabled");

    let event_loop = winit::event_loop::EventLoop::with_user_event()
        .build()
        .expect_universal("Event loop building failed");

    // setting event loop control flow does not seem to make WindowEvents fire faster
    // triggering WindowEvent::RedrawRequested in a loop, redraws occur 60 times per second no matter if it's ControlFlow::Poll or ControlFlow::Wait
    // setting this elsewhere doesn't seem to make a difference either
    // event_loop.set_control_flow(winit::event_loop::ControlFlow::Poll);

    #[cfg(not(target_arch = "wasm32"))]
    event_loop
        .run_app(&mut App::new())
        .expect_universal("Event loop run app failed");
    // some guides use run_app() for web as well, however, this is not recommended by winit 0.30+ docs
    // if run_app() was used for web, there would be an intentional JavaScript control flow error in the web console

    #[cfg(target_arch = "wasm32")]
    {
        let app = App::new(&event_loop);
        use winit::platform::web::EventLoopExtWebSys;
        event_loop.spawn_app(app);
    }
}

struct App {
    #[cfg(target_arch = "wasm32")]
    proxy: Option<winit::event_loop::EventLoopProxy<State>>, // used to send custom events to EventLoop
    state: Option<State>,
}

impl App {
    #[allow(clippy::missing_const_for_fn)] // create_proxy() isn't const
    fn new(
        #[cfg(target_arch = "wasm32")] event_loop: &winit::event_loop::EventLoop<State>,
    ) -> Self {
        Self {
            #[cfg(target_arch = "wasm32")]
            proxy: Some(event_loop.create_proxy()), // returns EventLoopProxy
            state: None,
        }
    }
}

impl winit::application::ApplicationHandler<State> for App {
    // called by winit when the window is resumed
    // seems to be called only once when the window is opened
    fn resumed(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        #[allow(unused_mut)]
        // window_attributes doesn't have to be mut on non-wasm, but must be mut on wasm
        let mut window_attributes = winit::window::Window::default_attributes();

        #[cfg(target_arch = "wasm32")]
        {
            // JsCast is a trait implemented by web_sys::Element used for .unchecked_into()
            use wasm_bindgen::JsCast;
            // WindowAttributesExtWebSys is a trait implemented by winit::window::WindowAttributes used for .with_canvas()
            use winit::platform::web::WindowAttributesExtWebSys;

            const CANVAS_ID: &str = "rust-insert-wasm"; // matches with index.html <canvas id="rust-insert-wasm">

            window_attributes = window_attributes.with_canvas(Some(
                wgpu::web_sys::window() // becomes Option<web_sys::Window>
                    .expect_universal("web_sys::Window unwrap failed") // becomes web_sys::Window
                    .document() // becomes Option<web_sys::Document>
                    .expect_universal("web_sys::Document unwrap failed") // becomes web_sys::Document
                    .get_element_by_id(CANVAS_ID) // becomes Option<web_sys::Element>
                    .expect_universal("web_sys::Element unwrap failed") // becomes web_sys::Element
                    .unchecked_into(), // becomes what .with_canvas() wants, a web_sys::HtmlCanvasElement
            ));
        }

        // create an Arc<Window>
        let window = std::sync::Arc::new(
            event_loop
                .create_window(window_attributes)
                .expect_universal("Event loop create_window failed"),
        );
        window.set_title("Path Tracer");

        #[cfg(not(target_arch = "wasm32"))]
        {
            // use pollster async runtime/executor to await the future from State::new(window)
            self.state = Some(
                pollster::block_on(State::new(
                    window,
                    event_loop.owned_display_handle(), // Supply the newly required display handle
                ))
                .expect_universal("State::new failed"),
            );
            // for non-wasm, this is where self.state is set, and initialization is finished
        }

        #[cfg(target_arch = "wasm32")]
        {
            if let Some(proxy) = self.proxy.take() {
                // self.proxy is of type Option<EventLoopProxy<State>>
                // .take() on Option<> makes the original value None, and returns the original value
                // so after .take(), self.proxy is None, and self.proxy's previous value is matched with Some(proxy)
                // matched with Some(proxy), proxy is EventLoopProxy<State> in this if let block

                // Fetch display handle here so it's not bound across the async bounds incorrectly
                let display_handle = event_loop.owned_display_handle();

                wasm_bindgen_futures::spawn_local(async move {
                    // use the browser's async runtime
                    // assert panics if what's inside it doesn't evaluate to true
                    assert!(
                        proxy
                            // this essentially is a call to user_event()
                            .send_event(
                                State::new(window, display_handle) // Passed display_handle here
                                    .await
                                    .expect_universal("State::new failed")
                            )
                            .is_ok()
                    ) // .is_ok() on Result<T, E> returns true if Result<T, E> is Ok(T)
                });
            }
        }
    }

    // called by winit when an event is sent from EventLoopProxy::send_event
    // EventLoopProxy<State>::send_event() is only called in resumed() under wasm32
    #[cfg(target_arch = "wasm32")]
    fn user_event(&mut self, _event_loop: &winit::event_loop::ActiveEventLoop, mut event: State) {
        // sometimes on initial web page load, neither render nor resize is called
        // so, force a resize call here, which triggers a render call too
        // on native, this doesn't seem to be necessary since a resize and render are both automatically called
        let size = event.window.inner_size();
        event.resize(size.width, size.height);
        self.state = Some(event); // for wasm, this is where self.state is set, and initialization is finished
    }

    // called by winit when the OS or browser sends an event to the window
    fn window_event(
        &mut self,
        event_loop: &winit::event_loop::ActiveEventLoop,
        _window_id: winit::window::WindowId,
        event: winit::event::WindowEvent,
    ) {
        if let Some(state) = &mut self.state {
            // self is of type: &mut App
            // &mut self.state is of type: &mut Option<State>
            // Some(state) matched with type &mut Option<State> automatically desugars to &mut Some(ref mut state)
            // self.state of type &mut Option<> is matched with &mut Some()
            // if this match is successful, what's inside self.state of type &mut Option<>, or just State, is bound to what's inside Some(), or just ref mut state
            // ref mut makes State, bounded to ref mut state, &mut State instead of just State
            // so, state in this block is of type: &mut State
            match event {
                winit::event::WindowEvent::CloseRequested => event_loop.exit(),
                winit::event::WindowEvent::Resized(size) => state.resize(size.width, size.height),
                winit::event::WindowEvent::RedrawRequested => state.update(),
                winit::event::WindowEvent::KeyboardInput { event, .. } => state.key_event(&event),
                // mouse button events are just used for locking and hiding cursor
                winit::event::WindowEvent::MouseInput {
                    state: mouse_state,
                    button,
                    ..
                } => state.mouse_button_event(mouse_state, button),
                _ => {}
            }
        } // if self.state is not Some() but is None, nothing is done
    }

    // device events are non-window specific, so they can be captured even when the window is not in focus
    // however, only when the mouse is over the window will mouse motion events be captured
    // mouse motion events for 3D camera control are better captured as device events
    fn device_event(
        &mut self,
        _event_loop: &winit::event_loop::ActiveEventLoop,
        _device_id: winit::event::DeviceId,
        event: winit::event::DeviceEvent,
    ) {
        if let Some(state) = &mut self.state
            && let winit::event::DeviceEvent::MouseMotion { delta } = event
        {
            state.mouse_move_event(delta);
        }
    }
}
