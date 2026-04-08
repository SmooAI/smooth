import { cn } from "../../lib/utils"

export function Tabs({ children, className, ...props }: React.HTMLAttributes<HTMLDivElement>) {
    return <div className={cn("", className)} {...props}>{children}</div>
}

export function TabsList({ children, className, ...props }: React.HTMLAttributes<HTMLDivElement>) {
    return (
        <div className={cn("inline-flex h-9 items-center justify-center rounded-lg bg-muted p-1 text-muted-foreground", className)} {...props}>
            {children}
        </div>
    )
}

export function TabsTrigger({
    active, onClick, children, className, ...props
}: React.HTMLAttributes<HTMLButtonElement> & { active?: boolean }) {
    return (
        <button
            className={cn(
                "inline-flex items-center justify-center whitespace-nowrap rounded-md px-3 py-1 text-sm font-medium ring-offset-background transition-all",
                active && "bg-background text-foreground shadow-sm",
                className
            )}
            onClick={onClick}
            {...props}
        >
            {children}
        </button>
    )
}
