//! The work, flowing (SMOODEV-3371).
//!
//! A particle layer over the queue's machine view (screen-blended, so it reads
//! as light and never hides text). Each particle is a sliver of work: it
//! leaves a waiting ticket, crosses to the gate, and, when the gate is open,
//! streams into a running job's lane and fades out at the job's progress edge.
//! While the pressure gate holds heavy jobs, most particles stall at the gate
//! and pile up in the hot end of the spectrum — the backlog made visible. A
//! lane whose job holds a shared lock runs gold.
//!
//! **It must never load the machine it is watching.** So:
//! - at most `MAX` particles, drawn at most `FPS` times a second;
//! - nothing runs while the tab is hidden or the view is scrolled out of sight;
//! - three.js (WebGL) only when the GPU is a real one: a software rasterizer
//!   (`failIfMajorPerformanceCaveat`) gets a small Canvas 2D painter instead,
//!   with fewer particles at a lower rate;
//! - `prefers-reduced-motion` gets no particles at all.
//!
//! The layer re-reads positions off the DOM it sits over (twice a second),
//! from `data-ciq-*` attributes, so the React view stays the single source of
//! layout: `data-ciq-gate` (the gate), `data-ciq-wait` (a waiting ticket) and
//! `data-ciq-run` (a running job; `data-lock`, `data-frac` 0–1). It is
//! decoration over a view that is complete without it.

import { useEffect, useRef } from 'react';
import * as THREE from 'three';

/** Particle cap and frame rate for the WebGL painter. */
const MAX = 360;
const FPS = 30;
/** The Canvas 2D fallback's (software-rendered, so leaner). */
const MAX_2D = 140;
const FPS_2D = 20;

const VERTEX = `
    attribute float aSize;
    attribute float aAlpha;
    attribute vec3 aColor;
    varying float vAlpha;
    varying vec3 vColor;
    uniform float uDpr;
    void main() {
        vAlpha = aAlpha;
        vColor = aColor;
        gl_PointSize = aSize * uDpr;
        gl_Position = projectionMatrix * modelViewMatrix * vec4(position, 1.0);
    }
`;

const FRAGMENT = `
    varying float vAlpha;
    varying vec3 vColor;
    void main() {
        vec2 d = gl_PointCoord - vec2(0.5);
        float r = length(d);
        if (r > 0.5) discard;
        float core = smoothstep(0.5, 0.05, r);
        gl_FragColor = vec4(vColor, vAlpha * core);
    }
`;

interface Rect {
    x: number;
    y: number;
    w: number;
    h: number;
}

interface Lane extends Rect {
    lock: boolean;
    frac: number;
}

interface Layout {
    w: number;
    h: number;
    gateX: number;
    /** True when the gate lies horizontally (narrow screens stack the view). */
    stacked: boolean;
    gateY: number;
    sources: Rect[];
    lanes: Lane[];
}

function measure(root: HTMLElement): Layout | null {
    const box = root.getBoundingClientRect();
    const gate = root.querySelector<HTMLElement>('[data-ciq-gate]');
    if (!gate || box.width === 0) return null;
    const rel = (el: Element): Rect => {
        const r = el.getBoundingClientRect();
        return { x: r.left - box.left, y: r.top - box.top, w: r.width, h: r.height };
    };
    const g = rel(gate);
    return {
        w: box.width,
        h: box.height,
        gateX: g.x + g.w / 2,
        gateY: g.y + g.h / 2,
        stacked: g.w > g.h,
        sources: [...root.querySelectorAll('[data-ciq-wait]')].slice(0, 12).map(rel),
        lanes: [...root.querySelectorAll<HTMLElement>('[data-ciq-run]')].map((el) => ({
            ...rel(el),
            lock: el.dataset.lock === '1',
            frac: Math.min(1, Math.max(0.08, Number(el.dataset.frac ?? '0.5'))),
        })),
    };
}

/** Struct-of-arrays particle state, shared by both painters. */
class Particles {
    readonly n: number;
    readonly x: Float32Array;
    readonly y: Float32Array;
    readonly sy: Float32Array;
    readonly speed: Float32Array;
    readonly age: Float32Array;
    readonly size: Float32Array;
    readonly alpha: Float32Array;
    /** 0 dead · 1 to the gate · 2 stalled at the gate · 3 in a lane */
    readonly stage: Uint8Array;
    readonly lane: Int16Array;
    /** 0 heat · 1 hot (stalled) · 2 gold (a locked lane) */
    readonly tone: Uint8Array;
    private debt = 0;

    constructor(n: number) {
        this.n = n;
        this.x = new Float32Array(n);
        this.y = new Float32Array(n);
        this.sy = new Float32Array(n);
        this.speed = new Float32Array(n);
        this.age = new Float32Array(n);
        this.size = new Float32Array(n);
        this.alpha = new Float32Array(n);
        this.stage = new Uint8Array(n);
        this.lane = new Int16Array(n);
        this.tone = new Uint8Array(n);
    }

    private spawn(L: Layout): void {
        for (let i = 0; i < this.n; i++) {
            if (this.stage[i] !== 0) continue;
            const src = L.sources.length ? L.sources[Math.floor(Math.random() * L.sources.length)] : null;
            if (L.stacked) {
                this.x[i] = src ? src.x + Math.random() * src.w : Math.random() * L.w;
                this.y[i] = src ? src.y + src.h - 4 : L.gateY - 80;
            } else {
                this.x[i] = src ? src.x + src.w - 6 : L.gateX - 180;
                this.y[i] = src ? src.y + src.h * (0.25 + Math.random() * 0.5) : L.h * (0.2 + Math.random() * 0.6);
            }
            this.sy[i] = this.y[i];
            this.speed[i] = 90 + Math.random() * 120;
            this.age[i] = 0;
            this.stage[i] = 1;
            this.lane[i] = -1;
            this.size[i] = 5 + Math.random() * 6;
            this.tone[i] = 0;
            return;
        }
    }

    /** Advance `dt` seconds. The arrival rate follows the backlog. */
    step(L: Layout, dt: number, held: boolean): void {
        this.debt += dt * Math.min(this.n / 2, 16 + L.sources.length * 5);
        while (this.debt >= 1) {
            this.spawn(L);
            this.debt -= 1;
        }
        for (let i = 0; i < this.n; i++) {
            if (this.stage[i] === 0) {
                this.alpha[i] = 0;
                continue;
            }
            this.age[i] += dt;
            const v = this.speed[i] * dt;
            if (this.stage[i] === 1) {
                const along = L.stacked ? L.gateY - this.y[i] : L.gateX - this.x[i];
                if (along > 6) {
                    if (L.stacked) {
                        this.y[i] += v;
                    } else {
                        this.x[i] += v;
                        this.y[i] += (this.sy[i] - this.y[i]) * 0.02;
                    }
                } else if (held && Math.random() < 0.72) {
                    this.stage[i] = 2;
                    this.tone[i] = 1;
                    this.age[i] = 0;
                } else if (L.lanes.length) {
                    this.stage[i] = 3;
                    this.lane[i] = Math.floor(Math.random() * L.lanes.length);
                    this.tone[i] = L.lanes[this.lane[i]].lock ? 2 : 0;
                    this.age[i] = 0;
                } else {
                    this.stage[i] = 2;
                    this.age[i] = 0;
                }
            } else if (this.stage[i] === 2) {
                // Stalled at the gate: a slow shiver, then gone.
                const shiver = Math.abs(Math.sin(this.age[i] * 3 + i)) * 26;
                if (L.stacked) {
                    this.y[i] = L.gateY - 4 - shiver;
                    this.x[i] += (Math.random() - 0.5) * 12 * dt;
                } else {
                    this.x[i] = L.gateX - 4 - shiver;
                    this.y[i] += (Math.random() - 0.5) * 12 * dt;
                }
                if (this.age[i] > 2.6) this.stage[i] = 0;
            } else {
                // Into a lane, then along it to the job's progress edge.
                const target = L.lanes[this.lane[i]];
                if (!target) {
                    this.stage[i] = 0;
                    continue;
                }
                const ty = target.y + target.h / 2;
                const endX = target.x + target.w * target.frac;
                const tx = Math.max(target.x + 4, Math.min(endX, this.x[i] + v * 1.6));
                this.x[i] += (tx - this.x[i]) * Math.min(1, dt * 6) + (this.x[i] < target.x ? v : 0);
                this.y[i] += (ty - this.y[i]) * Math.min(1, dt * 5);
                if (this.x[i] >= endX - 3 || this.age[i] > 4) this.stage[i] = 0;
            }
            const fadeIn = Math.min(1, this.age[i] * 4);
            this.alpha[i] = this.stage[i] === 2 ? 0.8 * fadeIn * Math.max(0, 1 - this.age[i] / 2.6) : 0.75 * fadeIn;
        }
    }
}

interface Palette {
    heat: string;
    hot: string;
    gold: string;
}

interface Painter {
    readonly max: number;
    readonly fps: number;
    resize(w: number, h: number): void;
    paint(p: Particles, colors: Palette): void;
    dispose(): void;
}

/** three.js points, additive: the full-quality painter on a real GPU. */
function webglPainter(el: HTMLElement): Painter | null {
    let renderer: THREE.WebGLRenderer;
    try {
        // A software rasterizer (no GPU, or a blocklisted one) fails here and
        // gets the 2D painter instead, rather than burning CPU on emulated GL.
        renderer = new THREE.WebGLRenderer({
            antialias: false,
            alpha: true,
            premultipliedAlpha: false,
            failIfMajorPerformanceCaveat: true,
            powerPreference: 'low-power',
        });
    } catch {
        return null;
    }
    const dpr = Math.min(2, window.devicePixelRatio || 1);
    renderer.setPixelRatio(dpr);
    renderer.setClearColor(0x000000, 0);
    el.appendChild(renderer.domElement);
    renderer.domElement.style.display = 'block';
    const scene = new THREE.Scene();
    const camera = new THREE.OrthographicCamera(0, 1, 0, 1, -1, 1);
    const geo = new THREE.BufferGeometry();
    const pos = new Float32Array(MAX * 3);
    const col = new Float32Array(MAX * 3);
    const sizes = new Float32Array(MAX);
    const alphas = new Float32Array(MAX);
    geo.setAttribute('position', new THREE.BufferAttribute(pos, 3));
    geo.setAttribute('aColor', new THREE.BufferAttribute(col, 3));
    geo.setAttribute('aSize', new THREE.BufferAttribute(sizes, 1));
    geo.setAttribute('aAlpha', new THREE.BufferAttribute(alphas, 1));
    const mat = new THREE.ShaderMaterial({
        vertexShader: VERTEX,
        fragmentShader: FRAGMENT,
        uniforms: { uDpr: { value: dpr } },
        transparent: true,
        depthWrite: false,
        blending: THREE.AdditiveBlending,
    });
    scene.add(new THREE.Points(geo, mat));
    const c = { heat: new THREE.Color(), hot: new THREE.Color(), gold: new THREE.Color() };
    return {
        max: MAX,
        fps: FPS,
        resize(w, h) {
            renderer.setSize(w, h, false);
            renderer.domElement.style.width = `${w}px`;
            renderer.domElement.style.height = `${h}px`;
            camera.right = w;
            camera.bottom = h;
            camera.updateProjectionMatrix();
        },
        paint(p, colors) {
            c.heat.set(colors.heat);
            c.hot.set(colors.hot);
            c.gold.set(colors.gold);
            for (let i = 0; i < p.n; i++) {
                const k = i * 3;
                pos[k] = p.x[i];
                pos[k + 1] = p.y[i];
                const cc = p.tone[i] === 1 ? c.hot : p.tone[i] === 2 ? c.gold : c.heat;
                col[k] = cc.r;
                col[k + 1] = cc.g;
                col[k + 2] = cc.b;
                sizes[i] = p.size[i];
                alphas[i] = p.alpha[i];
            }
            geo.attributes.position.needsUpdate = true;
            geo.attributes.aColor.needsUpdate = true;
            geo.attributes.aSize.needsUpdate = true;
            geo.attributes.aAlpha.needsUpdate = true;
            renderer.render(scene, camera);
        },
        dispose() {
            geo.dispose();
            mat.dispose();
            renderer.dispose();
            renderer.domElement.remove();
        },
    };
}

/** Canvas 2D circles: the lean painter for software rendering. */
function canvasPainter(el: HTMLElement): Painter | null {
    const canvas = document.createElement('canvas');
    const ctx = canvas.getContext('2d');
    if (!ctx) return null;
    canvas.style.display = 'block';
    el.appendChild(canvas);
    const dpr = Math.min(1.5, window.devicePixelRatio || 1);
    return {
        max: MAX_2D,
        fps: FPS_2D,
        resize(w, h) {
            canvas.width = Math.round(w * dpr);
            canvas.height = Math.round(h * dpr);
            canvas.style.width = `${w}px`;
            canvas.style.height = `${h}px`;
            ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
        },
        paint(p, colors) {
            ctx.clearRect(0, 0, canvas.width, canvas.height);
            ctx.globalCompositeOperation = 'lighter';
            for (let i = 0; i < p.n; i++) {
                if (p.alpha[i] <= 0.01) continue;
                ctx.globalAlpha = p.alpha[i] * 0.8;
                ctx.fillStyle = p.tone[i] === 1 ? colors.hot : p.tone[i] === 2 ? colors.gold : colors.heat;
                ctx.beginPath();
                ctx.arc(p.x[i], p.y[i], p.size[i] / 2.6, 0, Math.PI * 2);
                ctx.fill();
            }
            ctx.globalAlpha = 1;
        },
        dispose() {
            canvas.remove();
        },
    };
}

export function QueueFlow({ root, heat, holding, hot, gold }: { root: HTMLElement | null; heat: string; holding: boolean; hot: string; gold: string }) {
    const mount = useRef<HTMLDivElement>(null);
    const live = useRef({ heat, holding, hot, gold });

    useEffect(() => {
        live.current = { heat, holding, hot, gold };
    }, [heat, holding, hot, gold]);

    useEffect(() => {
        const el = mount.current;
        if (!el || !root) return;
        if (window.matchMedia('(prefers-reduced-motion: reduce)').matches) return;
        const painter = webglPainter(el) ?? canvasPainter(el);
        if (!painter) return;
        el.dataset.painter = painter.max === MAX ? 'webgl' : 'canvas2d';
        const particles = new Particles(painter.max);
        let layout: Layout | null = null;

        const resize = () => {
            const r = root.getBoundingClientRect();
            painter.resize(r.width, r.height);
            layout = measure(root);
        };
        resize();
        const ro = new ResizeObserver(resize);
        ro.observe(root);

        // Scrolled out of sight is as good as hidden.
        let onScreen = true;
        const io = new IntersectionObserver(([e]) => {
            onScreen = e?.isIntersecting ?? true;
        });
        io.observe(root);

        let raf = 0;
        let last = performance.now();
        let drawnAt = 0;
        // Re-read the layout a couple of times a second: jobs move between
        // snapshots, and the DOM is the only source of where they are.
        let measuredAt = 0;
        const minGap = 1000 / painter.fps;
        const frame = (now: number) => {
            raf = requestAnimationFrame(frame);
            if (document.visibilityState !== 'visible' || !onScreen) {
                last = now;
                return;
            }
            if (now - drawnAt < minGap) return;
            drawnAt = now;
            const dt = Math.min(0.1, (now - last) / 1000);
            last = now;
            if (now - measuredAt > 500) {
                layout = measure(root);
                measuredAt = now;
            }
            if (!layout) return;
            particles.step(layout, dt, live.current.holding);
            painter.paint(particles, live.current);
        };
        raf = requestAnimationFrame(frame);

        return () => {
            cancelAnimationFrame(raf);
            ro.disconnect();
            io.disconnect();
            painter.dispose();
        };
    }, [root]);

    return <div ref={mount} className="pointer-events-none absolute inset-0 z-20 mix-blend-screen" aria-hidden />;
}
