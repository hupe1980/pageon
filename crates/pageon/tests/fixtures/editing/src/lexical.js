import { createEditor, $getRoot, $createParagraphNode, $createTextNode } from 'lexical';
import { registerRichText, HeadingNode, QuoteNode } from '@lexical/rich-text';
import { registerHistory, createEmptyHistoryState } from '@lexical/history';
window.Lexical = { createEditor, registerRichText, registerHistory, createEmptyHistoryState, HeadingNode, QuoteNode, $getRoot, $createParagraphNode, $createTextNode };
