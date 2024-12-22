import './utils/theme';

import { Editor } from '@tiptap/core'
import StarterKit from '@tiptap/starter-kit'
import Placeholder from '@tiptap/extension-placeholder'

// Get editor element and its data attributes
const editorElement = document.querySelector('#editor');
const room = editorElement.dataset.room;
const initialContent = editorElement.dataset.initialContent;

// WebSocket setup
const ws = new WebSocket(`ws://${window.location.host}/ws/${room}`);
let isReceiving = false;

let content = '';

try {
    if(initialContent.length > 0) {
        content = JSON.parse(initialContent);
    }
} catch (error) {
    console.error('Error:', error);
}

function debounce(func, wait) {
    let timeout;
    return function executedFunction(...args) {
        const later = () => {
            clearTimeout(timeout);
            func(...args);
        };
        clearTimeout(timeout);
        timeout = setTimeout(later, wait);
    };
}

// Initialize Tiptap editor
const editor = new Editor({
    element: editorElement,
    extensions: [
        StarterKit,
        Placeholder.configure({
            placeholder: 'Start typing...',
        }),
    ],
    content,
    editorProps: {
        attributes: {
            class: '',
        },
    },
    onUpdate:debounce(({ editor }) => {
        if (!isReceiving) {
            const content = editor.getJSON();
            ws.send(JSON.stringify({ t: 'UpdateContent', c: { content: JSON.stringify(content) } }));
        }
    }, 300)
});

// WebSocket handlers
ws.onmessage = (event) => {
    isReceiving = true;
    let data = JSON.parse(event.data);
    let command = data.t;
    if (command === 'UpdatedContent') {
        editor.commands.setContent(JSON.parse(data.c.content));
        document.querySelector('#updatedAt').innerText = data.c.updated_at;
    } else if (command === 'RoomDeleted') {
        window.location.href = '/';
    }
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

const deleteButton = document.getElementById('deleteRoom');
if (deleteButton) {
    deleteButton.addEventListener('click', async () => {
        if (confirm('Are you sure you want to delete this room? This action cannot be undone.')) {
            ws.send(JSON.stringify({ t: 'DeleteRoom' }));
        }
    });
}
