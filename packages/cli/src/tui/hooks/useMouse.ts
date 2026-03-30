/** useMouse — mouse click support for Ink TUI
 *
 * Enables xterm mouse reporting via ANSI escape sequences.
 * Parses SGR (1006) mouse events for click position.
 *
 * Usage:
 *   const { mouseX, mouseY, clicked } = useMouse();
 *
 * The hook enables mouse tracking on mount and disables on unmount.
 * Reports click coordinates relative to the terminal viewport.
 */

import { useStdin } from 'ink';
import { useCallback, useEffect, useRef, useState } from 'react';

export interface MouseEvent {
    x: number;
    y: number;
    button: 'left' | 'right' | 'middle' | 'scroll-up' | 'scroll-down';
    type: 'press' | 'release' | 'move';
}

interface MouseState {
    x: number;
    y: number;
    clicked: boolean;
    lastEvent: MouseEvent | null;
}

export function useMouse(onMouseEvent?: (event: MouseEvent) => void) {
    const { stdin, setRawMode } = useStdin();
    const [state, setState] = useState<MouseState>({ x: 0, y: 0, clicked: false, lastEvent: null });
    const callbackRef = useRef(onMouseEvent);
    callbackRef.current = onMouseEvent;

    const handleData = useCallback((data: Buffer) => {
        const str = data.toString();

        // Parse SGR mouse events: ESC[<button;x;yM or ESC[<button;x;ym
        const ESC = String.fromCharCode(27);
        const sgrPattern = new RegExp(`${ESC.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}\\[<(\\d+);(\\d+);(\\d+)([Mm])`);
        const sgrMatch = str.match(sgrPattern);
        if (sgrMatch) {
            const buttonCode = parseInt(sgrMatch[1], 10);
            const x = parseInt(sgrMatch[2], 10);
            const y = parseInt(sgrMatch[3], 10);
            const isRelease = sgrMatch[4] === 'm';

            let button: MouseEvent['button'] = 'left';
            if ((buttonCode & 3) === 1) button = 'middle';
            else if ((buttonCode & 3) === 2) button = 'right';
            else if (buttonCode & 64) button = buttonCode & 1 ? 'scroll-down' : 'scroll-up';

            const type: MouseEvent['type'] = isRelease ? 'release' : buttonCode & 32 ? 'move' : 'press';

            const event: MouseEvent = { x, y, button, type };

            if (type === 'press') {
                setState({ x, y, clicked: true, lastEvent: event });
            } else {
                setState((prev) => ({ ...prev, lastEvent: event }));
            }

            callbackRef.current?.(event);
            return;
        }

        // Parse legacy mouse events: ESC[Mbxy
        const legacyPrefix = ESC + '[M';
        if (str.length >= 6 && str.startsWith(legacyPrefix)) {
            const buttonCode = str.charCodeAt(3) - 32;
            const x = str.charCodeAt(4) - 32;
            const y = str.charCodeAt(5) - 32;

            let button: MouseEvent['button'] = 'left';
            if ((buttonCode & 3) === 1) button = 'middle';
            else if ((buttonCode & 3) === 2) button = 'right';
            else if (buttonCode & 64) button = buttonCode & 1 ? 'scroll-down' : 'scroll-up';

            const type: MouseEvent['type'] = (buttonCode & 3) === 3 ? 'release' : 'press';
            const event: MouseEvent = { x, y, button, type };

            if (type === 'press') {
                setState({ x, y, clicked: true, lastEvent: event });
            }

            callbackRef.current?.(event);
        }
    }, []);

    useEffect(() => {
        if (!stdin) return;

        // Enable SGR mouse mode (better coordinates, supports > 223 columns)
        const ESC = String.fromCharCode(27);
        setRawMode(true);
        process.stdout.write(`${ESC}[?1000h`); // Enable mouse click tracking
        process.stdout.write(`${ESC}[?1006h`); // Enable SGR extended mouse mode

        stdin.on('data', handleData);

        return () => {
            stdin.off('data', handleData);
            // Disable mouse tracking
            process.stdout.write(`${ESC}[?1006l`);
            process.stdout.write(`${ESC}[?1000l`);
        };
    }, [stdin, setRawMode, handleData]);

    const resetClick = useCallback(() => {
        setState((prev) => ({ ...prev, clicked: false }));
    }, []);

    return { ...state, resetClick };
}

/** Check if a click is within a rectangular region */
export function isClickInRegion(mouseX: number, mouseY: number, regionX: number, regionY: number, width: number, height: number): boolean {
    return mouseX >= regionX && mouseX < regionX + width && mouseY >= regionY && mouseY < regionY + height;
}
