const fs = require('fs');
let code = fs.readFileSync('src/lib.rs', 'utf-8');

const findBlock = 
                    unsafe {
                        let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
                        SetWindowLongPtrW(
                            hwnd,
                            GWL_EXSTYLE,
                            ex_style | WS_EX_NOACTIVATE.0 as isize,
                        );
                    }
                    log::info!("Widget HWND configured with WS_EX_NOACTIVATE.");
                }
            };

const replaceBlock = 
                    unsafe {
                        let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
                        SetWindowLongPtrW(
                            hwnd,
                            GWL_EXSTYLE,
                            ex_style | WS_EX_NOACTIVATE.0 as isize | WS_EX_TRANSPARENT.0 as isize,
                        );
                    }
                    log::info!("Widget HWND configured with WS_EX_NOACTIVATE and WS_EX_TRANSPARENT.");
                }

                start_widget_hover_tracker_win(_widget.clone());
            };

code = code.replace('GWL_EXSTYLE, WS_EX_NOACTIVATE,', 'GWL_EXSTYLE, WS_EX_NOACTIVATE, WS_EX_TRANSPARENT,');
code = code.replace(findBlock, replaceBlock);

if (code.includes('start_widget_hover_tracker_win(_widget.clone());')) {
    fs.writeFileSync('src/lib.rs', code, 'utf-8');
} else {
    console.log("REPLACE FAILED");
}
