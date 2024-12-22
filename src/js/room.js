import 'uno.css';
import '../styles/editor.css';

import { Editor } from '@tiptap/core'
import StarterKit from '@tiptap/starter-kit'

// Get editor element and its data attributes
const editorElement = document.querySelector('#editor');
const room = editorElement.dataset.room;
const initialContent = editorElement.dataset.initialContent;

// WebSocket setup
const ws = new WebSocket(`ws://${window.location.host}/ws/${room}`);
let isReceiving = false;

// Initialize Tiptap editor
const editor = new Editor({
    element: editorElement,
    extensions: [
        StarterKit,
    ],
    content: initialContent ? JSON.parse(initialContent) : '',
    editorProps: {
        attributes: {
            class: 'prose prose-sm sm:prose lg:prose-lg max-w-none focus:outline-none',
        },
    },
    onUpdate: ({ editor }) => {
        if (!isReceiving) {
            const content = editor.getJSON();
            ws.send(JSON.stringify(content));
        }
    },
});

// WebSocket handlers
ws.onmessage = (event) => {
    isReceiving = true;
    editor.commands.setContent(JSON.parse(event.data));
    isReceiving = false;
};

// Toolbar button handlers
const toolbarActions = {
    bold: () => editor.chain().focus().toggleBold().run(),
    italic: () => editor.chain().focus().toggleItalic().run(),
    strike: () => editor.chain().focus().toggleStrike().run(),
    code: () => editor.chain().focus().toggleCode().run(),
    h1: () => editor.chain().focus().toggleHeading({ level: 1 }).run(),
    h2: () => editor.chain().focus().toggleHeading({ level: 2 }).run(),
    bullet: () => editor.chain().focus().toggleBulletList().run(),
    number: () => editor.chain().focus().toggleOrderedList().run(),
    quote: () => editor.chain().focus().toggleBlockquote().run(),
};

document.querySelectorAll('.editor-toolbar button').forEach(button => {
    button.addEventListener('click', () => {
        const type = button.dataset.type;
        if (toolbarActions[type]) {
            toolbarActions[type]();
            button.classList.toggle('is-active', editor.isActive(type));
        }
    });
});

// Update toolbar button states
editor.on('selectionUpdate', () => {
    document.querySelectorAll('.editor-toolbar button').forEach(button => {
        const type = button.dataset.type;
        button.classList.toggle('is-active', editor.isActive(type));
    });
});
