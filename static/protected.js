const roomId = document.getElementById('app').dataset.roomId;

// File names are user supplied: only ever inserted with textContent.
function renderFiles(files) {
    if (!files || files.length === 0) return;
    const list = document.getElementById('filesList');
    for (const file of files) {
        const a = document.createElement('a');
        a.className = 'file-link';
        a.href = `/api/room/${roomId}/files/${encodeURIComponent(file.id)}`;
        a.download = '';
        const name = document.createElement('span');
        name.className = 'name';
        name.textContent = file.name;
        name.title = file.name;
        const size = document.createElement('span');
        size.className = 'size';
        size.textContent = formatSize(file.size);
        a.append(name, size);
        list.append(a);
    }
    document.getElementById('filesBox').style.display = 'block';
}

function showError(message) {
    const error = document.getElementById('error');
    error.textContent = message;
    error.style.display = 'block';
}

async function unlock() {
    const password = document.getElementById('password').value;
    if (!password) return;

    try {
        const res = await fetch(`/api/room/${roomId}/unlock`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ password })
        });
        const data = await res.json().catch(() => ({}));

        if (res.ok && data.success) {
            document.getElementById('unlockForm').style.display = 'none';
            document.getElementById('contentBox').style.display = 'block';
            document.getElementById('content').textContent = data.content;
            renderFiles(data.files);

            const warning = document.getElementById('warning');
            if (data.last_view) {
                warning.style.display = 'block';
                warning.textContent = data.files && data.files.length
                    ? 'Dernière ouverture : la room est supprimée. Les fichiers restent téléchargeables 15 minutes.'
                    : 'Cette room a été supprimée après cette lecture.';
            } else if (data.views_remaining !== null && data.views_remaining !== undefined) {
                warning.style.display = 'block';
                warning.textContent = `${data.views_remaining} ouverture(s) restante(s)`;
            }
        } else if (res.status === 410) {
            showError('Cette room a expiré.');
        } else if (res.status === 429) {
            showError('Trop de tentatives, réessayez plus tard.');
        } else if (res.status === 401) {
            showError('Mot de passe incorrect');
        } else {
            showError(data.error || 'Erreur');
        }
    } catch (e) {
        showError('Erreur réseau');
    }
}

function copyRoomContent() {
    const content = document.getElementById('content').textContent;
    copyToClipboard(content, 'Contenu copié !');
}

document.getElementById('password').addEventListener('keypress', e => {
    if (e.key === 'Enter') unlock();
});

on('unlockBtn', 'click', unlock);
on('copyBtn', 'click', copyRoomContent);
