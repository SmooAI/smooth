//! The work, flowing (SMOODEV-3371).
//!
//! A three.js particle layer over the queue's machine view (screen-blended,
//! so it reads as light and never hides text). Each particle
//! is a sliver of work: it leaves a waiting ticket, crosses to the gate, and,
//! when the gate is open, streams into a running job's lane and fades out at
//! the job's progress edge. While the pressure gate holds heavy jobs, most
//! particles stall at the gate and pile up in the hot end of the spectrum —
//! the backlog made visible. A lane whose job holds a shared lock runs gold.
//!
//! The layer re-reads positions off the DOM it sits behind (twice a second), from
//! `data-ciq-*` attributes, so the React view stays the single source of
//! layout: `data-ciq-gate` (the gate), `data-ciq-wait` (a waiting ticket) and
//! `data-ciq-run` (a running job; `data-lock`, `data-frac` 0–1).
//!
//! It is decoration over a view that is complete without it: no WebGL, a
//! hidden tab, or `prefers-reduced-motion` means no particles, and nothing
//! else changes.

import { useEffect, useRef } from 'react';
import * as THREE from 'three';

const MAX = 900;

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

export function QueueFlow({ root, heat, holding, hot, gold }: { root: HTMLElement | null; heat: string; holding: boolean; hot: string; gold: string }) {
    const mount = useRef<HTMLDivElement>(null);
    const live = useRef({ heat, holding, hot, gold });
    const layoutRef = useRef<Layout | null>(null);

    useEffect(() => {
        live.current = { heat, holding, hot, gold };
    }, [heat, holding, hot, gold]);

    useEffect(() => {
        const el = mount.current;
        if (!el || !root) return;
        if (window.matchMedia('(prefers-reduced-motion: reduce)').matches) return;

        let renderer: THREE.WebGLRenderer;
        try {
            renderer = new THREE.WebGLRenderer({ antialias: false, alpha: true, premultipliedAlpha: false });
        } catch {
            return; // No WebGL: the view is complete without the particles.
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
        const size = new Float32Array(MAX);
        const alpha = new Float32Array(MAX);
        geo.setAttribute('position', new THREE.BufferAttribute(pos, 3));
        geo.setAttribute('aColor', new THREE.BufferAttribute(col, 3));
        geo.setAttribute('aSize', new THREE.BufferAttribute(size, 1));
        geo.setAttribute('aAlpha', new THREE.BufferAttribute(alpha, 1));
        const mat = new THREE.ShaderMaterial({
            vertexShader: VERTEX,
            fragmentShader: FRAGMENT,
            uniforms: { uDpr: { value: dpr } },
            transparent: true,
            depthWrite: false,
            blending: THREE.AdditiveBlending,
        });
        scene.add(new THREE.Points(geo, mat));

        // Particle state, struct-of-arrays.
        const px = new Float32Array(MAX);
        const py = new Float32Array(MAX);
        const speed = new Float32Array(MAX);
        const age = new Float32Array(MAX);
        const stage = new Uint8Array(MAX); // 0 dead · 1 to gate · 2 stalled at gate · 3 in lane
        const lane = new Int16Array(MAX);
        const sy = new Float32Array(MAX);
        const tone = new Uint8Array(MAX); // 0 heat · 1 hot (stalled) · 2 gold (lock lane)
        const c = { heat: new THREE.Color(), hot: new THREE.Color(), gold: new THREE.Color() };

        const resize = () => {
            const r = root.getBoundingClientRect();
            renderer.setSize(r.width, r.height, false);
            renderer.domElement.style.width = `${r.width}px`;
            renderer.domElement.style.height = `${r.height}px`;
            camera.right = r.width;
            camera.bottom = r.height;
            camera.updateProjectionMatrix();
            layoutRef.current = measure(root);
        };
        resize();
        const ro = new ResizeObserver(resize);
        ro.observe(root);

        const spawn = (L: Layout) => {
            for (let i = 0; i < MAX; i++) {
                if (stage[i] !== 0) continue;
                const src = L.sources.length ? L.sources[Math.floor(Math.random() * L.sources.length)] : null;
                if (L.stacked) {
                    px[i] = src ? src.x + Math.random() * src.w : Math.random() * L.w;
                    py[i] = src ? src.y + src.h - 4 : L.gateY - 80;
                } else {
                    px[i] = src ? src.x + src.w - 6 : L.gateX - 180;
                    py[i] = src ? src.y + src.h * (0.25 + Math.random() * 0.5) : L.h * (0.2 + Math.random() * 0.6);
                }
                sy[i] = py[i];
                speed[i] = 90 + Math.random() * 120;
                age[i] = 0;
                stage[i] = 1;
                lane[i] = -1;
                size[i] = 5 + Math.random() * 6;
                tone[i] = 0;
                return;
            }
        };

        let raf = 0;
        let last = performance.now();
        // Re-read the layout a couple of times a second: jobs move between
        // snapshots, and the DOM is the only source of where they are.
        let measuredAt = 0;
        let debt = 0;
        const frame = (now: number) => {
            raf = requestAnimationFrame(frame);
            const dt = Math.min(0.05, (now - last) / 1000);
            last = now;
            if (document.visibilityState !== 'visible') return;
            if (now - measuredAt > 400) {
                layoutRef.current = measure(root);
                measuredAt = now;
            }
            const L = layoutRef.current;
            if (!L) return;
            const { holding: held } = live.current;
            c.heat.set(live.current.heat);
            c.hot.set(live.current.hot);
            c.gold.set(live.current.gold);

            // Arrival rate follows the backlog.
            debt += dt * (40 + L.sources.length * 10);
            while (debt >= 1) {
                spawn(L);
                debt -= 1;
            }

            for (let i = 0; i < MAX; i++) {
                if (stage[i] === 0) {
                    alpha[i] = 0;
                    continue;
                }
                age[i] += dt;
                const v = speed[i] * dt;
                if (stage[i] === 1) {
                    // Toward the gate.
                    const along = L.stacked ? L.gateY - py[i] : L.gateX - px[i];
                    if (along > 6) {
                        if (L.stacked) {
                            py[i] += v;
                        } else {
                            px[i] += v;
                            py[i] += (sy[i] - py[i]) * 0.02;
                        }
                    } else if (held && Math.random() < 0.72) {
                        stage[i] = 2;
                        tone[i] = 1;
                        age[i] = 0;
                    } else if (L.lanes.length) {
                        stage[i] = 3;
                        lane[i] = Math.floor(Math.random() * L.lanes.length);
                        tone[i] = L.lanes[lane[i]].lock ? 2 : 0;
                        age[i] = 0;
                    } else {
                        stage[i] = 2;
                        age[i] = 0;
                    }
                } else if (stage[i] === 2) {
                    // Stalled at the gate: a slow shiver, then gone.
                    if (L.stacked) {
                        py[i] = L.gateY - 4 - Math.abs(Math.sin(age[i] * 3 + i)) * 26;
                        px[i] += (Math.random() - 0.5) * 12 * dt;
                    } else {
                        px[i] = L.gateX - 4 - Math.abs(Math.sin(age[i] * 3 + i)) * 26;
                        py[i] += (Math.random() - 0.5) * 12 * dt;
                    }
                    if (age[i] > 2.6) stage[i] = 0;
                } else {
                    // Into a lane, then along it to the job's progress edge.
                    const target = L.lanes[lane[i]];
                    if (!target) {
                        stage[i] = 0;
                        continue;
                    }
                    const ty = target.y + target.h / 2;
                    const endX = target.x + target.w * target.frac;
                    const tx = Math.max(target.x + 4, Math.min(endX, px[i] + v * 1.6));
                    px[i] += (tx - px[i]) * Math.min(1, dt * 6) + (px[i] < target.x ? v : 0);
                    py[i] += (ty - py[i]) * Math.min(1, dt * 5);
                    if (px[i] >= endX - 3 || age[i] > 4) stage[i] = 0;
                }
                const k = i * 3;
                pos[k] = px[i];
                pos[k + 1] = py[i];
                pos[k + 2] = 0;
                const cc = tone[i] === 1 ? c.hot : tone[i] === 2 ? c.gold : c.heat;
                col[k] = cc.r;
                col[k + 1] = cc.g;
                col[k + 2] = cc.b;
                const fadeIn = Math.min(1, age[i] * 4);
                alpha[i] = stage[i] === 2 ? 0.8 * fadeIn * Math.max(0, 1 - age[i] / 2.6) : 0.75 * fadeIn;
            }
            geo.attributes.position.needsUpdate = true;
            geo.attributes.aColor.needsUpdate = true;
            geo.attributes.aSize.needsUpdate = true;
            geo.attributes.aAlpha.needsUpdate = true;
            renderer.render(scene, camera);
        };
        raf = requestAnimationFrame(frame);

        return () => {
            cancelAnimationFrame(raf);
            ro.disconnect();
            geo.dispose();
            mat.dispose();
            renderer.dispose();
            renderer.domElement.remove();
        };
    }, [root]);

    return <div ref={mount} className="pointer-events-none absolute inset-0 z-20 mix-blend-screen" aria-hidden />;
}
