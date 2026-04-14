const fs = require('fs');
let code = fs.readFileSync('src/lib.rs', 'utf-8');
console.log('Includes Target 3?', code.includes('log::info!("Widget HWND configured with WS_EX_NOACTIVATE.");'));
console.log('Includes Target 2?', code.includes('ex_style | WS_EX_NOACTIVATE.0 as isize,'));
