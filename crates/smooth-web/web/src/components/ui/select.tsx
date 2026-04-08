import { cn } from "../../lib/utils"

export function Select({
    value, onChange, children, className, ...props
}: React.SelectHTMLAttributes<HTMLSelectElement>) {
    return (
        <select
            value={value}
            onChange={onChange}
            className={cn(
                "flex h-9 w-full items-center justify-between rounded-md border border-input bg-transparent px-3 py-2 text-sm shadow-sm ring-offset-background placeholder:text-muted-foreground focus:outline-none focus:ring-1 focus:ring-ring",
                className
            )}
            {...props}
        >
            {children}
        </select>
    )
}
