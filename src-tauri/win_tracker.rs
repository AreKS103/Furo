// ── Windows cursor-polling hover tracker ─────────────────────────────────

#[cfg(target_os = "windows")]
fn set_win_click_through(hwnd: windows::Win32::Foundation::HWND, ignore: bool) {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_TRANSPARENT,
    };
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let new_ex = if ignore {
            ex | WS_EX_TRANSPARENT.0 as isize
        } else {
            ex & !(WS_EX_TRANSPARENT.0 as isize)
        };
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, new_ex);
    }
}

#[cfg(target_os = "windows")]
fn start_widget_hover_tracker_win(widget: tauri::WebviewWindow) {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

    const POLL_MS: u64 = 50;
    const PILL_HALF_W: f64 = 20.0; // half of 40 px collapsed pill
    const PILL_H: f64 = 10.0;      // collapsed pill height
    const Y_PAD_TOP: f64 = 30.0;   // tolerance above pill
    const Y_PAD_BOT: f64 = 10.0;
    const EXIT_PAD: f64 = 10.0;    // exit-zone jitter margin

    let hwnd_raw: isize = match widget.hwnd() {
        Ok(h) => h.0 as isize,
        Err(_) => return,
    };

    std::thread::Builder::new()
        .name("furo-widget-hover-win".into())
        .spawn(move || {
            use windows::Win32::Foundation::HWND;
            let hwnd = HWND(hwnd_raw as *mut _);
            let mut was_hovering = false;

            loop {
                std::thread::sleep(std::time::Duration::from_millis(POLL_MS));

                let cursor = {
                    let mut pt = POINT { x: 0, y: 0 };
                    unsafe { let _ = GetCursorPos(&mut pt); }
                    pt
                };

                let (pos, size, scale) = match (
                    widget.outer_position(),
                    widget.outer_size(),
                    widget.scale_factor(),
                ) {
                    (Ok(p), Ok(s), Ok(sc)) => (p, s, sc),
                    _ => break,
                };

                let wx = pos.x as f64;
                let wy = pos.y as f64;
                let ww = size.width as f64;
                let wh = size.height as f64;
                let bottom_phys = wy + wh;
                let center_x = wx + ww / 2.0;

                let half_w = PILL_HALF_W * scale;
                let pill_h = PILL_H * scale;

                // Adjust these margins to cover the 40x10 pill gracefully
                let in_activation =
                    (cursor.x as f64 - center_x).abs() <= half_w + (EXIT_PAD * scale)
                        && cursor.y as f64 >= bottom_phys - pill_h - (Y_PAD_TOP * scale)
                        && cursor.y as f64 <= bottom_phys + (Y_PAD_BOT * scale);

                let ep = EXIT_PAD * scale;
                let in_exit =
                    cursor.x as f64 >= wx - ep
                        && cursor.x as f64 <= wx + ww + ep
                        && cursor.y as f64 >= wy - ep
                        && cursor.y as f64 <= wy + wh + ep;

                let is_hovering = if was_hovering { in_exit } else { in_activation };

                if is_hovering != was_hovering {
                    was_hovering = is_hovering;
                    set_win_click_through(hwnd, !is_hovering);
                    let _ = widget.emit("widget-hover", is_hovering);
                }
            }
            log::info!("Widget hover tracker (Windows) exited.");
        })
        .ok();
}
