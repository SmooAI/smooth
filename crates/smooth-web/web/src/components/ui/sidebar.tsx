import {
    createContext,
    useContext,
    useState,
    useEffect,
    useCallback,
    type ReactNode,
} from "react";
import { PanelLeft, X } from "lucide-react";
import { cn } from "../../lib/utils";
import { useIsMobile } from "../../hooks/use-mobile";

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const SIDEBAR_KEY = "smooth-sidebar-open";
const SIDEBAR_WIDTH = "16rem";
const SIDEBAR_WIDTH_MOBILE = "18rem";
const SIDEBAR_WIDTH_COLLAPSED = "0px";

// ---------------------------------------------------------------------------
// Context
// ---------------------------------------------------------------------------

interface SidebarContextValue {
    open: boolean;
    setOpen: (v: boolean) => void;
    toggle: () => void;
    isMobile: boolean;
}

const SidebarContext = createContext<SidebarContextValue | null>(null);

export function useSidebar() {
    const ctx = useContext(SidebarContext);
    if (!ctx) throw new Error("useSidebar must be used within <SidebarProvider>");
    return ctx;
}

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

export function SidebarProvider({ children }: { children: ReactNode }) {
    const isMobile = useIsMobile();
    const [open, setOpenState] = useState(() => {
        if (typeof window === "undefined") return true;
        const stored = localStorage.getItem(SIDEBAR_KEY);
        return stored === null ? true : stored === "true";
    });

    const setOpen = useCallback(
        (v: boolean) => {
            setOpenState(v);
            if (!isMobile) {
                localStorage.setItem(SIDEBAR_KEY, String(v));
            }
        },
        [isMobile],
    );

    const toggle = useCallback(() => setOpen(!open), [open, setOpen]);

    // Close mobile sidebar when switching to desktop
    useEffect(() => {
        if (!isMobile) {
            // Restore persisted desktop state
            const stored = localStorage.getItem(SIDEBAR_KEY);
            setOpenState(stored === null ? true : stored === "true");
        } else {
            // Mobile always starts closed
            setOpenState(false);
        }
    }, [isMobile]);

    // Cmd+B / Ctrl+B keyboard shortcut
    useEffect(() => {
        const handler = (e: KeyboardEvent) => {
            if ((e.metaKey || e.ctrlKey) && e.key === "b") {
                e.preventDefault();
                toggle();
            }
        };
        window.addEventListener("keydown", handler);
        return () => window.removeEventListener("keydown", handler);
    }, [toggle]);

    return (
        <SidebarContext.Provider value={{ open, setOpen, toggle, isMobile }}>
            <div
                className="flex min-h-screen w-full"
                style={
                    {
                        "--sidebar-width": SIDEBAR_WIDTH,
                        "--sidebar-width-mobile": SIDEBAR_WIDTH_MOBILE,
                    } as React.CSSProperties
                }
            >
                {children}
            </div>
        </SidebarContext.Provider>
    );
}

// ---------------------------------------------------------------------------
// Sidebar
// ---------------------------------------------------------------------------

export function Sidebar({ children, className }: { children: ReactNode; className?: string }) {
    const { open, setOpen, isMobile } = useSidebar();

    // Mobile: overlay drawer
    if (isMobile) {
        return (
            <>
                {/* Backdrop */}
                {open && (
                    <div
                        className="fixed inset-0 z-40 bg-black/60 transition-opacity"
                        onClick={() => setOpen(false)}
                    />
                )}
                {/* Drawer */}
                <aside
                    className={cn(
                        "fixed inset-y-0 left-0 z-50 flex flex-col bg-sidebar text-sidebar-foreground border-r border-sidebar-border",
                        "w-[var(--sidebar-width-mobile)]",
                        "transition-transform duration-300 ease-in-out",
                        open ? "translate-x-0" : "-translate-x-full",
                        className,
                    )}
                >
                    {/* Close button */}
                    <button
                        onClick={() => setOpen(false)}
                        className="absolute right-3 top-3 rounded-md p-1 text-sidebar-foreground/60 hover:text-sidebar-foreground hover:bg-sidebar-accent transition-colors"
                    >
                        <X size={18} />
                    </button>
                    {children}
                </aside>
            </>
        );
    }

    // Desktop: fixed sidebar with collapse transition
    return (
        <aside
            className={cn(
                "relative flex flex-col bg-sidebar text-sidebar-foreground border-r border-sidebar-border shrink-0 overflow-hidden",
                "transition-[width] duration-300 ease-in-out",
                className,
            )}
            style={{ width: open ? SIDEBAR_WIDTH : SIDEBAR_WIDTH_COLLAPSED }}
        >
            <div
                className={cn(
                    "flex flex-col h-full",
                    "transition-opacity duration-200",
                    open ? "opacity-100" : "opacity-0 pointer-events-none",
                )}
                style={{ width: SIDEBAR_WIDTH }}
            >
                {children}
            </div>
        </aside>
    );
}

// ---------------------------------------------------------------------------
// SidebarHeader
// ---------------------------------------------------------------------------

export function SidebarHeader({ children, className }: { children: ReactNode; className?: string }) {
    return (
        <div className={cn("flex flex-col gap-2 p-4", className)}>
            {children}
        </div>
    );
}

// ---------------------------------------------------------------------------
// SidebarContent
// ---------------------------------------------------------------------------

export function SidebarContent({ children, className }: { children: ReactNode; className?: string }) {
    return (
        <div className={cn("flex-1 overflow-y-auto px-3 py-2", className)}>
            {children}
        </div>
    );
}

// ---------------------------------------------------------------------------
// SidebarTrigger
// ---------------------------------------------------------------------------

export function SidebarTrigger({ className }: { className?: string }) {
    const { toggle } = useSidebar();
    return (
        <button
            onClick={toggle}
            className={cn(
                "inline-flex items-center justify-center rounded-md p-2 text-muted-foreground hover:text-foreground hover:bg-accent transition-colors",
                className,
            )}
            aria-label="Toggle sidebar"
        >
            <PanelLeft size={18} />
        </button>
    );
}

// ---------------------------------------------------------------------------
// SidebarInset (main content area)
// ---------------------------------------------------------------------------

export function SidebarInset({ children, className }: { children: ReactNode; className?: string }) {
    return (
        <div className={cn("flex flex-1 flex-col min-w-0", className)}>
            {children}
        </div>
    );
}
