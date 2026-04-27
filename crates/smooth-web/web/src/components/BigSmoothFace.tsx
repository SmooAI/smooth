import { useEffect, useRef } from 'react';
import * as THREE from 'three';

// Gradient stops match the "th" half of the Smooth wordmark logo.
// (See crates/smooth-cli/src/gradient.rs — TH_START / TH_END.)
const TH_TOP = '#00a6a6';
const TH_BOTTOM = '#1238dd';

type FaceState = 'idle' | 'thinking';

interface BigSmoothFaceProps {
    state: FaceState;
    size?: number;
}

const VERTEX_SHADER = `
    varying vec3 vPos;
    varying vec3 vNormal;
    void main() {
        vPos = position;
        vNormal = normalize(normalMatrix * normal);
        gl_Position = projectionMatrix * modelViewMatrix * vec4(position, 1.0);
    }
`;

const FRAGMENT_SHADER = `
    uniform vec3 uColorTop;
    uniform vec3 uColorBottom;
    uniform float uPulse;
    uniform float uTime;
    varying vec3 vPos;
    varying vec3 vNormal;
    void main() {
        float t = clamp((vPos.y + 1.0) * 0.5, 0.0, 1.0);
        vec3 base = mix(uColorBottom, uColorTop, t);
        float lambert = clamp(dot(vNormal, normalize(vec3(0.4, 0.7, 0.9))), 0.0, 1.0);
        float shade = 0.55 + 0.55 * lambert;
        vec3 lit = base * shade;
        // Cool rim light along the silhouette
        float rim = pow(1.0 - clamp(vNormal.z, 0.0, 1.0), 2.5);
        lit += rim * mix(uColorTop, vec3(1.0, 1.0, 1.0), 0.35) * 0.6;
        // Pulse — subtle when idle, stronger when thinking
        lit *= 1.0 + uPulse * 0.18;
        gl_FragColor = vec4(lit, 1.0);
    }
`;

export function BigSmoothFace({ state, size = 96 }: BigSmoothFaceProps) {
    const containerRef = useRef<HTMLDivElement>(null);
    const stateRef = useRef<FaceState>(state);

    useEffect(() => {
        stateRef.current = state;
    }, [state]);

    useEffect(() => {
        const container = containerRef.current;
        if (!container) return;

        const scene = new THREE.Scene();
        const camera = new THREE.PerspectiveCamera(35, 1, 0.1, 100);
        camera.position.set(0, 0, 4.6);

        const renderer = new THREE.WebGLRenderer({ antialias: true, alpha: true });
        renderer.setSize(size, size);
        renderer.setPixelRatio(Math.min(window.devicePixelRatio, 2));
        container.appendChild(renderer.domElement);

        const headMat = new THREE.ShaderMaterial({
            uniforms: {
                uTime: { value: 0 },
                uColorTop: { value: new THREE.Color(TH_TOP) },
                uColorBottom: { value: new THREE.Color(TH_BOTTOM) },
                uPulse: { value: 0 },
            },
            vertexShader: VERTEX_SHADER,
            fragmentShader: FRAGMENT_SHADER,
        });
        const headGeom = new THREE.IcosahedronGeometry(1, 4);
        const head = new THREE.Mesh(headGeom, headMat);

        const faceGroup = new THREE.Group();
        faceGroup.add(head);

        // === Mouth — a confident smirk arc, slightly thicker so it
        // reads at small sizes. Drawn first so the sunglasses + hat
        // can overlap on top.
        const mouthGeom = new THREE.TorusGeometry(0.26, 0.046, 12, 48, Math.PI * 0.85);
        const mouthMat = new THREE.MeshBasicMaterial({ color: 0x020618 });
        const mouth = new THREE.Mesh(mouthGeom, mouthMat);
        mouth.position.set(0.04, -0.30, 0.86);
        // Arc opens upward (smirk) with a tilt for personality.
        mouth.rotation.z = Math.PI - 0.28;
        faceGroup.add(mouth);

        // === Sunglasses — two slim flat lenses + a thin bridge,
        // black with a teal-tinted highlight.
        const sunglasses = new THREE.Group();
        // Lens: a flattened disk facing forward.
        const lensGeom = new THREE.CircleGeometry(0.20, 28);
        const lensMat = new THREE.MeshBasicMaterial({ color: 0x080812 });
        const leftLens = new THREE.Mesh(lensGeom, lensMat);
        const rightLens = new THREE.Mesh(lensGeom, lensMat);
        // Squash horizontally → slim aviator-ish lens.
        leftLens.scale.set(1.2, 0.78, 1);
        rightLens.scale.set(1.2, 0.78, 1);
        leftLens.position.set(-0.34, 0.18, 0.94);
        rightLens.position.set(0.34, 0.18, 0.94);
        // Slight outward tilt — wraparound look.
        leftLens.rotation.y = 0.12;
        rightLens.rotation.y = -0.12;
        sunglasses.add(leftLens);
        sunglasses.add(rightLens);

        // Bridge between lenses.
        const bridgeGeom = new THREE.BoxGeometry(0.18, 0.04, 0.04);
        const bridgeMat = new THREE.MeshBasicMaterial({ color: 0x080812 });
        const bridge = new THREE.Mesh(bridgeGeom, bridgeMat);
        bridge.position.set(0, 0.20, 0.92);
        sunglasses.add(bridge);

        // Top frame line — a single thin slab that visually unites the lenses.
        const topFrameGeom = new THREE.BoxGeometry(1.05, 0.035, 0.04);
        const topFrame = new THREE.Mesh(topFrameGeom, bridgeMat);
        topFrame.position.set(0, 0.30, 0.92);
        sunglasses.add(topFrame);

        // Lens highlights — one short white-ish slash on each lens for
        // that "cool reflective shades" gleam.
        const glintGeom = new THREE.PlaneGeometry(0.12, 0.024);
        const glintMat = new THREE.MeshBasicMaterial({
            color: 0xffffff,
            transparent: true,
            opacity: 0.85,
        });
        const leftGlint = new THREE.Mesh(glintGeom, glintMat);
        const rightGlint = new THREE.Mesh(glintGeom, glintMat);
        leftGlint.position.set(-0.30, 0.24, 0.97);
        rightGlint.position.set(0.38, 0.24, 0.97);
        leftGlint.rotation.z = -0.55;
        rightGlint.rotation.z = -0.55;
        sunglasses.add(leftGlint);
        sunglasses.add(rightGlint);

        faceGroup.add(sunglasses);

        // === Hat — a low fedora-style cap. Crown is a flattened
        // cylinder; brim is a wide flat disk. Sits on top of the
        // head with a slight cool tilt.
        const hat = new THREE.Group();
        const crownGeom = new THREE.CylinderGeometry(0.55, 0.62, 0.34, 28, 1, false);
        const hatMat = new THREE.MeshBasicMaterial({ color: 0x060a18 });
        const crown = new THREE.Mesh(crownGeom, hatMat);
        crown.position.set(0, 0.92, 0);
        hat.add(crown);

        // Brim — a flat ring (thin cylinder) wider than the crown.
        const brimGeom = new THREE.CylinderGeometry(0.92, 0.95, 0.06, 36, 1, false);
        const brim = new THREE.Mesh(brimGeom, hatMat);
        brim.position.set(0, 0.74, 0);
        hat.add(brim);

        // Hat band — a thin teal stripe to tie back to the gradient.
        const bandGeom = new THREE.CylinderGeometry(0.56, 0.56, 0.07, 28, 1, true);
        const bandMat = new THREE.MeshBasicMaterial({ color: 0x00a6a6, side: THREE.DoubleSide });
        const band = new THREE.Mesh(bandGeom, bandMat);
        band.position.set(0, 0.78, 0);
        hat.add(band);

        // Cool tilt — back-and-to-the-left.
        hat.rotation.set(-0.08, 0, 0.14);
        hat.position.set(-0.05, -0.04, 0);
        faceGroup.add(hat);

        scene.add(faceGroup);

        const start = performance.now();
        let raf = 0;
        // Brief "lens flash" on the sunglasses every few seconds.
        let nextFlashAt = start + 2200 + Math.random() * 1800;
        let flashUntil = 0;

        const tick = () => {
            const now = performance.now();
            const t = (now - start) / 1000;
            const isThinking = stateRef.current === 'thinking';

            // Cool tilt is a small persistent z-roll when idle; thinking
            // straightens up and bobs forward more.
            const yawSpeed = isThinking ? 1.4 : 0.55;
            const yaw = Math.sin(t * yawSpeed) * (isThinking ? 0.55 : 0.28);
            const pitch = isThinking ? Math.sin(t * 2.6) * 0.14 : Math.sin(t * 0.9) * 0.06;
            const tilt = isThinking ? Math.sin(t * 0.9) * 0.04 : 0.12;
            faceGroup.rotation.set(pitch, yaw, tilt);

            // Bob — gentle when idle, more energetic when thinking.
            faceGroup.position.y = isThinking ? Math.sin(t * 4.2) * 0.05 : Math.sin(t * 1.4) * 0.03;

            // Lens flash — quickly brighten the glints to imply a
            // reflective sheen sliding across the shades.
            if (now > nextFlashAt && flashUntil === 0) {
                flashUntil = now + 260;
                const cadence = isThinking ? 1400 : 2800;
                nextFlashAt = now + cadence + Math.random() * 1600;
            }
            const flashing = flashUntil > 0 && now < flashUntil;
            if (!flashing && flashUntil > 0 && now >= flashUntil) flashUntil = 0;
            const flashAmt = flashing ? 1.0 : 0.55;
            glintMat.opacity = flashAmt;

            // Pulse the head shader.
            headMat.uniforms.uPulse.value = isThinking
                ? 0.45 + 0.55 * Math.sin(t * 4.2)
                : 0.18 * Math.sin(t * 1.3);
            headMat.uniforms.uTime.value = t;

            // While thinking, mouth opens slightly and bobs — feels
            // like he's mid-sentence.
            const mouthScale = isThinking ? 1.0 + Math.sin(t * 6) * 0.10 : 1.0;
            mouth.scale.set(mouthScale, mouthScale, 1);

            renderer.render(scene, camera);
            raf = requestAnimationFrame(tick);
        };
        raf = requestAnimationFrame(tick);

        const canvas = renderer.domElement;
        return () => {
            cancelAnimationFrame(raf);
            headGeom.dispose();
            headMat.dispose();
            mouthGeom.dispose();
            mouthMat.dispose();
            lensGeom.dispose();
            lensMat.dispose();
            bridgeGeom.dispose();
            bridgeMat.dispose();
            topFrameGeom.dispose();
            glintGeom.dispose();
            glintMat.dispose();
            crownGeom.dispose();
            hatMat.dispose();
            brimGeom.dispose();
            bandGeom.dispose();
            bandMat.dispose();
            renderer.dispose();
            if (canvas.parentNode === container) container.removeChild(canvas);
        };
    }, [size]);

    return (
        <div
            ref={containerRef}
            style={{
                width: size,
                height: size,
                lineHeight: 0,
                filter: state === 'thinking' ? 'drop-shadow(0 0 12px rgba(0,166,166,0.55))' : 'drop-shadow(0 0 6px rgba(18,56,221,0.35))',
                transition: 'filter 280ms ease',
            }}
            aria-label={state === 'thinking' ? 'Big Smooth is thinking' : 'Big Smooth'}
        />
    );
}
