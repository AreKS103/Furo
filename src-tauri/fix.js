const fs = require('fs');
const file = 'C:/Users/alpla/OneDrive/Documents/APP/Furo/src/components/FloatingWidget.tsx';
let txt = fs.readFileSync(file, 'utf8');

const regex = /style=\{\{\r?\n\s*bottom:\s*\"22px\",\s*transform:\s*\"translateX\(-50%\)\",(?:.|\r?\n)*?(?=pointerEvents:)/;
const match = txt.match(regex);
if (match) {
    const newStyle = \style={{
            bottom: "22px",
            transform: showPopup ? "translateX(-50%) translateY(0) scale(1)" : "translateX(-50%) translateY(15px) scale(0.9)",
            opacity: showPopup ? 1 : 0,
            transition: showPopup
              ? \\\opacity \ \, transform \ \\\\
              : \\\opacity 180ms \, transform 180ms \\\\,
            \;
    txt = txt.replace(match[0], newStyle);
    fs.writeFileSync(file, txt);
    console.log("Success");
} else {
    console.log("Not found");
}
