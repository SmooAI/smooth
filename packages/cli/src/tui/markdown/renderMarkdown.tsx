/** Core markdown renderer — converts marked tokens to Ink components
 *
 * Designed for streaming AI output:
 * - Idempotent rendering (re-render on each chunk)
 * - Graceful handling of partial markdown
 * - Modular token→component mapping
 */

import { Box, Text } from 'ink';
import { Lexer, type Token, type Tokens } from 'marked';
import React from 'react';

import { renderCodeBlock } from './renderCodeBlock.js';
import { renderInlineTokens } from './renderInline.js';
import { renderList } from './renderList.js';
import { DEFAULT_THEME, type MarkdownTheme } from './types.js';

/** Parse markdown string and render as Ink components */
export function renderMarkdown(markdown: string, theme: MarkdownTheme = DEFAULT_THEME): React.ReactNode {
    // Graceful handling of partial markdown — lexer won't crash on incomplete input
    let tokens: Token[];
    try {
        tokens = new Lexer().lex(markdown);
    } catch {
        // Fallback: render as plain text if parsing fails (partial stream)
        return <Text>{markdown}</Text>;
    }

    return <Box flexDirection="column">{tokens.map((token, i) => renderToken(token, theme, i))}</Box>;
}

/** Render a single block-level token */
export function renderToken(token: Token, theme: MarkdownTheme, key: number): React.ReactNode {
    switch (token.type) {
        case 'heading': {
            const heading = token as Tokens.Heading;
            const style = heading.depth === 1 ? theme.heading1 : heading.depth === 2 ? theme.heading2 : theme.heading3;
            return (
                <Box key={key} marginBottom={1} marginTop={key > 0 ? 1 : 0}>
                    <Text bold={style.bold} color={style.color}>
                        {renderInlineTokens(heading.tokens, theme, key)}
                    </Text>
                </Box>
            );
        }

        case 'paragraph': {
            const para = token as Tokens.Paragraph;
            return (
                <Box key={key} marginBottom={1}>
                    <Text color={theme.paragraph.color}>{renderInlineTokens(para.tokens, theme, key)}</Text>
                </Box>
            );
        }

        case 'list':
            return renderList(token as Tokens.List, theme, key);

        case 'code':
            return renderCodeBlock(token as Tokens.Code, theme, key);

        case 'blockquote': {
            const bq = token as Tokens.Blockquote;
            return (
                <Box key={key} borderLeft borderColor={theme.blockquote.borderColor} paddingLeft={1} marginBottom={1}>
                    <Text color={theme.blockquote.color} italic>
                        {bq.tokens.map((t, i) => renderToken(t, theme, i))}
                    </Text>
                </Box>
            );
        }

        case 'hr':
            return (
                <Box key={key} marginY={1}>
                    <Text color={theme.hr.color}>{'─'.repeat(40)}</Text>
                </Box>
            );

        case 'table': {
            const table = token as Tokens.Table;
            return renderTable(table, theme, key);
        }

        case 'space':
            return null;

        case 'html':
            // Strip HTML tags, render text only
            return <Text key={key}>{(token as Tokens.HTML).text.replace(/<[^>]*>/g, '')}</Text>;

        default:
            if ('text' in token) {
                return <Text key={key}>{(token as { text: string }).text}</Text>;
            }
            return null;
    }
}

/** Render a GFM table */
function renderTable(table: Tokens.Table, theme: MarkdownTheme, key: number): React.ReactNode {
    // Calculate column widths
    const colWidths = table.header.map((h, i) => {
        let max = h.text.length;
        for (const row of table.rows) {
            if (row[i]) max = Math.max(max, row[i].text.length);
        }
        return Math.min(max + 2, 40);
    });

    const pad = (text: string, width: number) => text.slice(0, width).padEnd(width);
    const separator = colWidths.map((w) => '─'.repeat(w)).join('┼');

    return (
        <Box key={key} flexDirection="column" marginBottom={1}>
            {/* Header */}
            <Text bold>{table.header.map((h, i) => pad(h.text, colWidths[i])).join('│')}</Text>
            <Text dimColor>{separator}</Text>
            {/* Rows */}
            {table.rows.map((row, ri) => (
                <Text key={ri}>{row.map((cell, ci) => pad(cell.text, colWidths[ci])).join('│')}</Text>
            ))}
        </Box>
    );
}
