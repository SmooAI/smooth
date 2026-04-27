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

        // Eyes — small inky spheres mounted just in front of the surface so
        // they pop visually against the gradient.
        const eyeGeom = new THREE.SphereGeometry(0.11, 20, 20);
        const eyeMat = new THREE.MeshBasicMaterial({ color: 0x020618 });
        const leftEye = new THREE.Mesh(eyeGeom, eyeMat);
        const rightEye = new THREE.Mesh(eyeGeom, eyeMat);
        leftEye.position.set(-0.32, 0.22, 0.86);
        rightEye.position.set(0.32, 0.22, 0.86);
        faceGroup.add(leftEye);
        faceGroup.add(rightEye);

        // Glints — give the eyes a pinprick of life and "coolness".
        const glintGeom = new THREE.SphereGeometry(0.028, 10, 10);
        const glintMat = new THREE.MeshBasicMaterial({ color: 0xffffff });
        const leftGlint = new THREE.Mesh(glintGeom, glintMat);
        const rightGlint = new THREE.Mesh(glintGeom, glintMat);
        leftGlint.position.set(-0.295, 0.255, 0.965);
        rightGlint.position.set(0.345, 0.255, 0.965);
        faceGroup.add(leftGlint);
        faceGroup.add(rightGlint);

        // Mouth — a partial torus arc forming a slight upturned smirk.
        const mouthGeom = new THREE.TorusGeometry(0.24, 0.028, 10, 36, Math.PI * 0.7);
        const mouthMat = new THREE.MeshBasicMaterial({ color: 0x020618 });
        const mouth = new THREE.Mesh(mouthGeom, mouthMat);
        mouth.position.set(0.02, -0.28, 0.82);
        // arc opens upward (smile/smirk) with a slight tilt
        mouth.rotation.z = Math.PI - 0.32;
        faceGroup.add(mouth);

        scene.add(faceGroup);

        const start = performance.now();
        let raf = 0;
        // Eye-blink scheduling: queue a quick blink every few seconds.
        let nextBlinkAt = start + 2400 + Math.random() * 2200;
        let blinkUntil = 0;

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

            // Eye blinks: scale Y down briefly. Blink more often while
            // thinking to suggest active attention.
            if (now > nextBlinkAt && blinkUntil === 0) {
                blinkUntil = now + 130;
                const cadence = isThinking ? 1100 : 2400;
                nextBlinkAt = now + cadence + Math.random() * 1800;
            }
            const blinking = blinkUntil > 0 && now < blinkUntil;
            if (!blinking && blinkUntil > 0 && now >= blinkUntil) blinkUntil = 0;
            const eyeY = blinking ? 0.08 : 1;
            leftEye.scale.y = eyeY;
            rightEye.scale.y = eyeY;
            leftGlint.visible = !blinking;
            rightGlint.visible = !blinking;

            // Pulse the head shader.
            headMat.uniforms.uPulse.value = isThinking
                ? 0.45 + 0.55 * Math.sin(t * 4.2)
                : 0.18 * Math.sin(t * 1.3);
            headMat.uniforms.uTime.value = t;

            // While thinking, eyes drift side-to-side a hair to suggest
            // scanning. Idle keeps them locked forward (cool).
            const drift = isThinking ? Math.sin(t * 1.7) * 0.04 : 0;
            leftEye.position.x = -0.32 + drift;
            rightEye.position.x = 0.32 + drift;
            leftGlint.position.x = -0.295 + drift;
            rightGlint.position.x = 0.345 + drift;

            renderer.render(scene, camera);
            raf = requestAnimationFrame(tick);
        };
        raf = requestAnimationFrame(tick);

        const canvas = renderer.domElement;
        return () => {
            cancelAnimationFrame(raf);
            headGeom.dispose();
            headMat.dispose();
            eyeGeom.dispose();
            eyeMat.dispose();
            glintGeom.dispose();
            glintMat.dispose();
            mouthGeom.dispose();
            mouthMat.dispose();
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
