// Pure builders for the Chrome DevTools Protocol input events behind the optional
// "trusted input" tier. No chrome.* calls here: background.js sends what these return,
// and scripts/chrome-page-smoke.mjs replays them against headless Chrome to prove the
// page sees real (isTrusted) clicks, keys and text.
const SmoothCdp = (() => {
    // Input.dispatch*Event modifier bits.
    const MOD = { Alt: 1, Control: 2, Meta: 4, Shift: 8 };
    const MOD_ALIASES = { ctrl: 'Control', control: 'Control', alt: 'Alt', option: 'Alt', meta: 'Meta', cmd: 'Meta', command: 'Meta', shift: 'Shift' };
    const BUTTONS = { left: 1, right: 2, middle: 4 };

    // Named keys: DOM key, code, Windows virtual key code, and the text a keyDown inserts.
    const KEYS = {
        Enter: { code: 'Enter', keyCode: 13, text: '\r' },
        Tab: { code: 'Tab', keyCode: 9 },
        Escape: { code: 'Escape', keyCode: 27 },
        Backspace: { code: 'Backspace', keyCode: 8 },
        Delete: { code: 'Delete', keyCode: 46 },
        ' ': { code: 'Space', keyCode: 32, text: ' ' },
        ArrowUp: { code: 'ArrowUp', keyCode: 38 },
        ArrowDown: { code: 'ArrowDown', keyCode: 40 },
        ArrowLeft: { code: 'ArrowLeft', keyCode: 37 },
        ArrowRight: { code: 'ArrowRight', keyCode: 39 },
        Home: { code: 'Home', keyCode: 36 },
        End: { code: 'End', keyCode: 35 },
        PageUp: { code: 'PageUp', keyCode: 33 },
        PageDown: { code: 'PageDown', keyCode: 34 },
        ...Object.fromEntries(Array.from({ length: 12 }, (_, i) => [`F${i + 1}`, { code: `F${i + 1}`, keyCode: 112 + i }])),
    };
    const KEY_ALIASES = { esc: 'Escape', return: 'Enter', space: ' ', del: 'Delete', up: 'ArrowUp', down: 'ArrowDown', left: 'ArrowLeft', right: 'ArrowRight' };

    // On macOS a synthetic key does nothing in a text field unless it carries the editing
    // command Chrome's Cocoa layer would have produced (Claude in Chrome found this too).
    const MAC_COMMANDS = {
        Backspace: 'deleteBackward',
        Delete: 'deleteForward',
        Enter: 'insertNewline',
        ArrowUp: 'moveUp',
        ArrowDown: 'moveDown',
        ArrowLeft: 'moveLeft',
        ArrowRight: 'moveRight',
        Home: 'moveToBeginningOfLine',
        End: 'moveToEndOfLine',
        PageUp: 'scrollPageUp',
        PageDown: 'scrollPageDown',
    };
    const MAC_META_COMMANDS = { a: 'selectAll', c: 'copy', x: 'cut', v: 'paste', z: 'undo' };

    function canonicalKey(k) {
        if (KEYS[k]) return k;
        const alias = KEY_ALIASES[k.toLowerCase()];
        if (alias) return alias;
        const named = Object.keys(KEYS).find((n) => n.toLowerCase() === k.toLowerCase());
        return named ?? k;
    }

    /** "Control+Shift+a" → { key: 'a', modifiers: 10, mods: ['Control', 'Shift'] }. A lone "+" is the plus key. */
    function parseCombo(combo) {
        const raw = String(combo);
        const parts = raw === '+' ? ['+'] : raw.endsWith('++') ? [...raw.slice(0, -2).split('+'), '+'] : raw.split('+');
        const key = canonicalKey(parts.pop());
        const mods = [];
        for (const p of parts) {
            const m = MOD[p] ? p : MOD_ALIASES[p.toLowerCase()];
            if (!m) throw new Error(`unknown modifier "${p}" in "${raw}" (use Control, Alt, Shift, Meta)`);
            if (!mods.includes(m)) mods.push(m);
        }
        return { key, mods, modifiers: mods.reduce((bits, m) => bits | MOD[m], 0) };
    }

    function keyInfo(key) {
        if (KEYS[key]) return { key, ...KEYS[key] };
        if (key.length === 1) {
            if (/[a-z]/i.test(key)) return { key, code: `Key${key.toUpperCase()}`, keyCode: key.toUpperCase().charCodeAt(0), text: key };
            if (/\d/.test(key)) return { key, code: `Digit${key}`, keyCode: key.charCodeAt(0), text: key };
            return { key, code: '', keyCode: 0, text: key };
        }
        throw new Error(`unknown key "${key}": use Enter, Tab, Escape, Backspace, Delete, Arrow*, Home, End, PageUp, PageDown, F1–F12 or one character`);
    }

    /** Input.dispatchKeyEvent params for pressing `combo` once: keyDown (or rawKeyDown) then keyUp. */
    function keyEvents(combo, { mac = false } = {}) {
        const { key, mods, modifiers } = parseCombo(combo);
        const info = keyInfo(key);
        // A chord with Control/Alt/Meta types nothing; Shift alone still does.
        const chord = mods.some((m) => m !== 'Shift');
        let text = chord ? undefined : info.text;
        if (text && mods.includes('Shift') && key.length === 1) text = key.toUpperCase();
        const commands = [];
        if (mac) {
            if (MAC_COMMANDS[key] && !chord) commands.push(MAC_COMMANDS[key]);
            if (mods.includes('Meta') && MAC_META_COMMANDS[key.toLowerCase()]) {
                commands.push(key.toLowerCase() === 'z' && mods.includes('Shift') ? 'redo' : MAC_META_COMMANDS[key.toLowerCase()]);
            }
        }
        const base = {
            modifiers,
            key: text && key.length === 1 ? text : info.key,
            code: info.code,
            windowsVirtualKeyCode: info.keyCode,
            nativeVirtualKeyCode: info.keyCode,
        };
        const down = { ...base, type: text ? 'keyDown' : 'rawKeyDown' };
        if (text) Object.assign(down, { text, unmodifiedText: text });
        if (commands.length) down.commands = commands;
        return [down, { ...base, type: 'keyUp' }];
    }

    /**
     * The CDP calls that type `text` into whatever has focus: runs of text go through
     * Input.insertText (one input event per run, which React and rich editors accept);
     * each newline is a real Enter key.
     */
    function typeCalls(text, { mac = false } = {}) {
        const calls = [];
        for (const [i, chunk] of String(text).split(/\r?\n/).entries()) {
            if (i > 0) for (const p of keyEvents('Enter', { mac })) calls.push(['Input.dispatchKeyEvent', p]);
            if (chunk) calls.push(['Input.insertText', { text: chunk }]);
        }
        return calls;
    }

    /** Input.dispatchMouseEvent params for a click (count 2 = double, 3 = triple) at CSS pixel (x, y). */
    function clickEvents(x, y, { button = 'left', count = 1, modifiers = 0 } = {}) {
        const bit = BUTTONS[button];
        if (!bit) throw new Error(`button must be left, right or middle, not "${button}"`);
        const n = Math.max(1, Math.min(3, Math.trunc(count)));
        const out = [{ type: 'mouseMoved', x, y, button: 'none', buttons: 0, modifiers }];
        for (let i = 1; i <= n; i++) {
            out.push({ type: 'mousePressed', x, y, button, buttons: bit, clickCount: i, modifiers });
            out.push({ type: 'mouseReleased', x, y, button, buttons: 0, clickCount: i, modifiers });
        }
        return out;
    }

    const hoverEvents = (x, y) => [{ type: 'mouseMoved', x, y, button: 'none', buttons: 0, modifiers: 0 }];

    /** A wheel scroll at (x, y): positive dy scrolls down. */
    const wheelEvents = (x, y, dx, dy) => [{ type: 'mouseWheel', x, y, deltaX: dx, deltaY: dy, modifiers: 0 }];

    /**
     * Page.captureScreenshot params that capture the visible viewport scaled to CSS pixels,
     * so a point in the image is the point to click. `m` is Page.getLayoutMetrics' result.
     */
    function viewportClip(m, devicePixelRatio) {
        const v = m.cssVisualViewport ?? m.visualViewport;
        return {
            format: 'png',
            captureBeyondViewport: false,
            fromSurface: true,
            clip: { x: v.pageX, y: v.pageY, width: v.clientWidth, height: v.clientHeight, scale: 1 / (devicePixelRatio || 1) },
        };
    }

    /** Refuse a point outside the viewport the last screenshot showed. */
    function checkPoint(x, y, width, height) {
        if (![x, y].every(Number.isFinite)) throw new Error('x and y must be numbers (CSS pixels, as in browse_screenshot)');
        if (x < 0 || y < 0 || x > width + 2 || y > height + 2) {
            throw new Error(`(${x}, ${y}) is outside the ${width}×${height} viewport: take a new browse_screenshot`);
        }
    }

    return { MOD, parseCombo, keyEvents, typeCalls, clickEvents, hoverEvents, wheelEvents, viewportClip, checkPoint };
})();

if (typeof module !== 'undefined') module.exports = SmoothCdp;
