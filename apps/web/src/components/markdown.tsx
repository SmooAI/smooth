'use client';

import type { Components } from 'react-markdown';

import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';

interface MarkdownProps {
    children: string;
    className?: string;
}

const components: Components = {
    h1: (props) => <h1 className="text-xl font-bold mb-2">{props.children}</h1>,
    h2: (props) => <h2 className="text-lg font-semibold mb-2">{props.children}</h2>,
    h3: (props) => <h3 className="text-base font-semibold mb-1">{props.children}</h3>,
    p: (props) => <p className="mb-2 leading-relaxed">{props.children}</p>,
    ul: (props) => <ul className="list-disc ml-4 mb-2 space-y-1">{props.children}</ul>,
    ol: (props) => <ol className="list-decimal ml-4 mb-2 space-y-1">{props.children}</ol>,
    li: (props) => <li className="leading-relaxed">{props.children}</li>,
    strong: (props) => <strong className="font-semibold text-neutral-100">{props.children}</strong>,
    code: (props) => {
        const isInline = !props.className;
        if (isInline) {
            return <code className="bg-neutral-800 px-1.5 py-0.5 rounded text-sm text-cyan-300 font-mono">{props.children}</code>;
        }
        return (
            <pre className="bg-neutral-800/50 border border-neutral-700 rounded-md p-3 mb-2 overflow-x-auto">
                <code className="text-sm font-mono text-neutral-200">{props.children}</code>
            </pre>
        );
    },
    pre: (props) => <>{props.children}</>,
    table: (props) => (
        <div className="overflow-x-auto mb-2">
            <table className="min-w-full border-collapse border border-neutral-700 text-sm">{props.children}</table>
        </div>
    ),
    th: (props) => <th className="border border-neutral-700 bg-neutral-800 px-3 py-1.5 text-left font-semibold">{props.children}</th>,
    td: (props) => <td className="border border-neutral-700 px-3 py-1.5">{props.children}</td>,
    a: (props) => (
        <a href={props.href} className="text-cyan-400 hover:text-cyan-300 underline" target="_blank" rel="noopener noreferrer">
            {props.children}
        </a>
    ),
    blockquote: (props) => <blockquote className="border-l-2 border-neutral-600 pl-3 italic text-neutral-400 mb-2">{props.children}</blockquote>,
    hr: () => <hr className="border-neutral-700 my-3" />,
};

export default function Markdown({ children, className }: MarkdownProps) {
    return (
        <div className={className}>
            <ReactMarkdown remarkPlugins={[remarkGfm]} components={components}>
                {children}
            </ReactMarkdown>
        </div>
    );
}
