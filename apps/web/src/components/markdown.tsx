'use client';

import type { Components } from 'react-markdown';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';

interface MarkdownProps {
    children: string;
    className?: string;
}

export default function Markdown({ children, className }: MarkdownProps) {
    const components: Components = {
        h1: ({ children }) => <h1 className="text-xl font-bold mb-2">{children}</h1>,
        h2: ({ children }) => <h2 className="text-lg font-semibold mb-2">{children}</h2>,
        h3: ({ children }) => <h3 className="text-base font-semibold mb-1">{children}</h3>,
        p: ({ children }) => <p className="mb-2 leading-relaxed">{children}</p>,
        ul: ({ children }) => <ul className="list-disc ml-4 mb-2 space-y-1">{children}</ul>,
        ol: ({ children }) => <ol className="list-decimal ml-4 mb-2 space-y-1">{children}</ol>,
        li: ({ children }) => <li className="leading-relaxed">{children}</li>,
        strong: ({ children }) => <strong className="font-semibold text-neutral-100">{children}</strong>,
        code: ({ children, className: codeClass }) => {
            const isInline = !codeClass;
            if (isInline) {
                return <code className="bg-neutral-800 px-1.5 py-0.5 rounded text-sm text-cyan-300 font-mono">{children}</code>;
            }
            return (
                <pre className="bg-neutral-800/50 border border-neutral-700 rounded-md p-3 mb-2 overflow-x-auto">
                    <code className="text-sm font-mono text-neutral-200">{children}</code>
                </pre>
            );
        },
        pre: ({ children }) => <>{children}</>,
        table: ({ children }) => (
            <div className="overflow-x-auto mb-2">
                <table className="min-w-full border-collapse border border-neutral-700 text-sm">{children}</table>
            </div>
        ),
        th: ({ children }) => <th className="border border-neutral-700 bg-neutral-800 px-3 py-1.5 text-left font-semibold">{children}</th>,
        td: ({ children }) => <td className="border border-neutral-700 px-3 py-1.5">{children}</td>,
        a: ({ children, href }) => (
            <a href={href} className="text-cyan-400 hover:text-cyan-300 underline" target="_blank" rel="noopener noreferrer">
                {children}
            </a>
        ),
        blockquote: ({ children }) => <blockquote className="border-l-2 border-neutral-600 pl-3 italic text-neutral-400 mb-2">{children}</blockquote>,
        hr: () => <hr className="border-neutral-700 my-3" />,
    };

    return (
        <div className={className}>
            <ReactMarkdown remarkPlugins={[remarkGfm]} components={components}>
                {children}
            </ReactMarkdown>
        </div>
    );
}
