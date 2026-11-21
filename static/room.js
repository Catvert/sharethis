const config = document.getElementById('app').dataset;
const roomId = config.roomId;
const maxFileSize = Number(config.maxFileSize);
const p2pThreshold = Number(config.p2pThreshold);
const maxContentSize = Number(config.maxContentSize);

const editor = document.getElementById('editor');
const status = document.getElementById('status');
const userCount = document.getElementById('userCount');
const saveModal = document.getElementById('saveModal');
const dropZone = document.getElementById('dropZone');
const fileInput = document.getElementById('fileInput');
const filesList = document.getElementById('filesList');
const filesCount = document.getElementById('filesCount');
const encoder = new TextEncoder();

let ws;
let userId;
let isUpdating = false;
let sealed = false;
let files = [];
const p2pFiles = new Map(); // fileId -> File (kept locally, never uploaded)
const peerConnections = new Map(); // peerId -> RTCPeerConnection

// ============================================================================
// WebSocket
// ============================================================================

function send(message) {
    if (ws?.readyState === WebSocket.OPEN) {
        ws.send(JSON.stringify(message));
        return true;
    }
    return false;
}

function onSealed() {
    if (sealed) return;
    sealed = true;
    editor.readOnly = true;
    showToast('Room protégée, rechargement…');
    setTimeout(() => location.reload(), 1200);
}

function connect() {
    const protocol = location.protocol === 'https:' ? 'wss:' : 'ws:';
    ws = new WebSocket(`${protocol}//${location.host}/ws/${roomId}`);

    ws.onopen = () => status.classList.add('connected');
    ws.onclose = async () => {
        status.classList.remove('connected');
        if (sealed) return;
        // The room may have been protected while we were away.
        try {
            const info = await (await fetch(`/api/room/${roomId}/info`)).json();
            if (info.is_protected) {
                onSealed();
                return;
            }
        } catch (e) {}
        setTimeout(connect, 2000);
    };

    ws.onmessage = (event) => {
        const data = JSON.parse(event.data);
        switch (data.type) {
            case 'init':
                userId = data.userId;
                editor.value = data.content;
                userCount.textContent = data.users;
                loadFiles();
                break;
            case 'content': {
                isUpdating = true;
                const start = editor.selectionStart;
                const end = editor.selectionEnd;
                editor.value = data.content;
                editor.setSelectionRange(start, end);
                isUpdating = false;
                break;
            }
            case 'users':
                userCount.textContent = data.count;
                break;
            case 'file_added':
                addFileToList(data.file);
                break;
            case 'file_removed':
                removeFileFromList(data.file_id);
                break;
            case 'p2p_signal':
                handleP2PSignal(data.from, data.signal);
                break;
            case 'p2p_request':
                handleP2PRequest(data.from, data.file_id);
                break;
            case 'sealed':
                onSealed();
                break;
        }
    };
}

// ============================================================================
// Editor
// ============================================================================

function sendContent() {
    if (encoder.encode(editor.value).length > maxContentSize) {
        showToast(`Texte trop long (max ${formatSize(maxContentSize)}), non synchronisé`, true);
        return;
    }
    send({ type: 'content', content: editor.value });
}

editor.addEventListener('input', () => {
    if (!isUpdating) sendContent();
});

function copyLink() {
    copyToClipboard(window.location.href, 'Lien copié !');
}

function copyContent() {
    copyToClipboard(editor.value, 'Contenu copié !');
}

function clearEditor() {
    editor.value = '';
    sendContent();
}

// ============================================================================
// Save Modal
// ============================================================================

function showSaveModal() {
    saveModal.classList.add('show');
}

function closeSaveModal() {
    saveModal.classList.remove('show');
}

async function saveRoom() {
    const password = document.getElementById('savePassword').value;
    const maxViews = document.getElementById('maxViews').value;
    const expiresIn = document.getElementById('expiresIn').value;
    const persistent = document.getElementById('persistentCheck').checked;

    if (!password) {
        showToast('Mot de passe requis', true);
        return;
    }

    try {
        const res = await fetch(`/api/room/${roomId}/save`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({
                password,
                max_views: maxViews ? parseInt(maxViews, 10) : null,
                expires_in: parseInt(expiresIn, 10),
                persistent
            })
        });

        if (res.ok) {
            closeSaveModal();
            onSealed();
            return;
        }
        const data = await res.json().catch(() => ({}));
        showToast(data.error || 'Erreur de sauvegarde', true);
    } catch (e) {
        showToast('Erreur réseau', true);
    }
}

// ============================================================================
// Files
// ============================================================================

async function loadFiles() {
    try {
        const res = await fetch(`/api/room/${roomId}/files`);
        if (!res.ok) return;
        files = await res.json();
        renderFiles();
    } catch (e) {
        console.error('Failed to load files:', e);
    }
}

function renderFiles() {
    filesList.replaceChildren(...files.map(createFileElement));
    filesCount.textContent = files.length;
}

function addFileToList(file) {
    if (!files.find(f => f.id === file.id)) {
        files.push(file);
        renderFiles();
    }
}

function removeFileFromList(fileId) {
    files = files.filter(f => f.id !== fileId);
    p2pFiles.delete(fileId);
    renderFiles();
}

function element(tag, className, text) {
    const el = document.createElement(tag);
    if (className) el.className = className;
    if (text !== undefined) el.textContent = text;
    return el;
}

// File names come from other users: only ever inserted with textContent.
function createFileElement(file) {
    const div = element('div', 'file-item');
    div.dataset.fileId = file.id;

    const info = element('div', 'file-info');
    const name = element('div', 'file-name', file.name);
    name.title = file.name;
    const meta = element('div', 'file-meta');
    meta.append(element('span', null, formatSize(file.size)));
    if (file.is_p2p) meta.append(element('span', 'file-p2p', 'P2P'));
    info.append(name, meta);

    const actions = element('div', 'file-actions');
    const download = element('button', 'file-btn download', '↓');
    download.title = 'Télécharger';
    download.addEventListener('click', () => downloadFile(file));
    const remove = element('button', 'file-btn delete', '×');
    remove.title = 'Supprimer';
    remove.addEventListener('click', () => deleteFile(file.id));
    actions.append(download, remove);

    div.append(element('span', 'file-icon', getFileIcon(file.mime_type)), info, actions);
    return div;
}

function getFileIcon(mimeType) {
    if (mimeType.startsWith('image/')) return '🖼';
    if (mimeType.startsWith('video/')) return '🎬';
    if (mimeType.startsWith('audio/')) return '🎵';
    if (mimeType.includes('pdf')) return '📄';
    if (mimeType.includes('zip') || mimeType.includes('rar') || mimeType.includes('tar')) return '📦';
    if (mimeType.includes('text') || mimeType.includes('javascript') || mimeType.includes('json')) return '📝';
    return '📎';
}

// ============================================================================
// Drag & Drop
// ============================================================================

dropZone.addEventListener('dragover', (e) => {
    e.preventDefault();
    dropZone.classList.add('dragover');
});

dropZone.addEventListener('dragleave', () => {
    dropZone.classList.remove('dragover');
});

dropZone.addEventListener('drop', (e) => {
    e.preventDefault();
    dropZone.classList.remove('dragover');
    handleFiles(e.dataTransfer.files);
});

fileInput.addEventListener('change', (e) => {
    handleFiles(e.target.files);
    fileInput.value = '';
});

async function handleFiles(fileList) {
    for (const file of fileList) {
        await uploadFile(file);
    }
}

// Files above the P2P threshold never leave the browser: only their name and
// size are announced, the content is sent directly to peers over WebRTC.
async function uploadFile(file) {
    const isP2P = file.size > p2pThreshold;
    if (!isP2P && file.size > maxFileSize) {
        showToast(`${file.name} trop volumineux (max ${formatSize(maxFileSize)})`, true);
        return;
    }

    const formData = new FormData();
    if (userId) formData.append('owner_id', userId);
    if (isP2P) {
        formData.append('p2p_name', file.name);
        formData.append('p2p_size', String(file.size));
    } else {
        formData.append('file', file);
    }

    try {
        const res = await fetch(`/api/room/${roomId}/files`, {
            method: 'POST',
            body: formData
        });
        const result = await res.json().catch(() => ({}));
        if (res.ok) {
            if (result.file.is_p2p) p2pFiles.set(result.file.id, file);
            showToast(isP2P ? `${file.name} partagé en P2P` : `${file.name} envoyé !`);
        } else {
            showToast(result.error || 'Erreur upload', true);
        }
    } catch (e) {
        showToast('Erreur réseau', true);
    }
}

function downloadFile(file) {
    if (file.is_p2p) {
        const local = p2pFiles.get(file.id);
        if (local) {
            downloadBlob(local, local.name);
            return;
        }
        if (send({ type: 'p2p_request', file_id: file.id })) {
            showToast('Demande P2P envoyée...');
        }
        return;
    }
    const a = document.createElement('a');
    a.href = `/api/room/${roomId}/files/${encodeURIComponent(file.id)}`;
    a.download = '';
    a.click();
}

function downloadBlob(blob, filename) {
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = filename;
    a.click();
    setTimeout(() => URL.revokeObjectURL(url), 10000);
}

async function deleteFile(fileId) {
    try {
        const res = await fetch(`/api/room/${roomId}/files/${encodeURIComponent(fileId)}`, {
            method: 'DELETE'
        });
        if (res.ok) {
            p2pFiles.delete(fileId);
            showToast('Fichier supprimé');
        } else {
            const data = await res.json().catch(() => ({}));
            showToast(data.error || 'Erreur suppression', true);
        }
    } catch (e) {
        showToast('Erreur suppression', true);
    }
}

// ============================================================================
// P2P WebRTC
// ============================================================================

const rtcConfig = {
    iceServers: [
        { urls: 'stun:stun.l.google.com:19302' }
    ]
};
const CHUNK_SIZE = 16 * 1024;
const READ_SIZE = 1024 * 1024;
const MAX_BUFFERED = 8 * 1024 * 1024;

function sendSignal(to, signal) {
    send({ type: 'p2p_signal', to, signal });
}

function closePeer(peerId) {
    const pc = peerConnections.get(peerId);
    if (pc) pc.close();
    peerConnections.delete(peerId);
}

function waitForDrain(channel) {
    return new Promise(resolve => {
        channel.onbufferedamountlow = () => {
            channel.onbufferedamountlow = null;
            resolve();
        };
    });
}

async function sendFileOverChannel(channel, file) {
    channel.bufferedAmountLowThreshold = MAX_BUFFERED / 4;
    channel.send(JSON.stringify({ type: 'meta', name: file.name, size: file.size }));
    for (let offset = 0; offset < file.size; offset += READ_SIZE) {
        const block = await file.slice(offset, offset + READ_SIZE).arrayBuffer();
        for (let i = 0; i < block.byteLength; i += CHUNK_SIZE) {
            if (channel.readyState !== 'open') return;
            if (channel.bufferedAmount > MAX_BUFFERED) await waitForDrain(channel);
            channel.send(block.slice(i, i + CHUNK_SIZE));
        }
    }
    channel.send(JSON.stringify({ type: 'done' }));
}

async function handleP2PRequest(fromPeer, fileId) {
    // Someone wants a file we own
    const file = p2pFiles.get(fileId);
    if (!file) return;

    closePeer(fromPeer);
    const pc = new RTCPeerConnection(rtcConfig);
    peerConnections.set(fromPeer, pc);

    const channel = pc.createDataChannel('file');
    channel.binaryType = 'arraybuffer';
    channel.onopen = () => {
        sendFileOverChannel(channel, file).catch(e => console.error('P2P send failed:', e));
    };
    channel.onclose = () => closePeer(fromPeer);

    pc.onicecandidate = (e) => {
        if (e.candidate) sendSignal(fromPeer, { type: 'candidate', candidate: e.candidate });
    };

    try {
        const offer = await pc.createOffer();
        await pc.setLocalDescription(offer);
        sendSignal(fromPeer, { type: 'offer', sdp: offer });
    } catch (e) {
        console.error('P2P offer failed:', e);
        closePeer(fromPeer);
    }
}

async function handleP2PSignal(fromPeer, signal) {
    try {
        if (signal.type === 'offer') {
            // Receiving an offer - we requested a file
            closePeer(fromPeer);
            const pc = new RTCPeerConnection(rtcConfig);
            peerConnections.set(fromPeer, pc);

            let fileMeta = null;
            let chunks = [];

            pc.ondatachannel = (e) => {
                const channel = e.channel;
                channel.binaryType = 'arraybuffer';

                channel.onmessage = (msg) => {
                    if (typeof msg.data === 'string') {
                        const data = JSON.parse(msg.data);
                        if (data.type === 'meta') {
                            fileMeta = data;
                            showToast(`Réception : ${data.name}`);
                        } else if (data.type === 'done' && fileMeta) {
                            downloadBlob(new Blob(chunks), fileMeta.name);
                            chunks = [];
                            showToast('Téléchargement P2P terminé !');
                            closePeer(fromPeer);
                        }
                    } else {
                        chunks.push(msg.data);
                    }
                };
            };

            pc.onicecandidate = (e) => {
                if (e.candidate) sendSignal(fromPeer, { type: 'candidate', candidate: e.candidate });
            };

            await pc.setRemoteDescription(new RTCSessionDescription(signal.sdp));
            const answer = await pc.createAnswer();
            await pc.setLocalDescription(answer);
            sendSignal(fromPeer, { type: 'answer', sdp: answer });

        } else if (signal.type === 'answer') {
            const pc = peerConnections.get(fromPeer);
            if (pc) await pc.setRemoteDescription(new RTCSessionDescription(signal.sdp));

        } else if (signal.type === 'candidate') {
            const pc = peerConnections.get(fromPeer);
            if (pc) await pc.addIceCandidate(new RTCIceCandidate(signal.candidate));
        }
    } catch (e) {
        console.error('P2P signaling failed:', e);
    }
}

// ============================================================================
// Init
// ============================================================================

on('roomIdBadge', 'click', copyLink);
on('shareBtn', 'click', copyLink);
on('clearBtn', 'click', clearEditor);
on('copyBtn', 'click', copyContent);
on('saveBtn', 'click', showSaveModal);
on('cancelSaveBtn', 'click', closeSaveModal);
on('confirmSaveBtn', 'click', saveRoom);
dropZone.addEventListener('click', () => fileInput.click());
fileInput.addEventListener('click', (e) => e.stopPropagation());

connect();
