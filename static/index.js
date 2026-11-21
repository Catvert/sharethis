function joinRoom() {
    const id = document.getElementById('roomId').value.trim();
    if (id) window.location.href = '/room/' + encodeURIComponent(id);
}

document.getElementById('roomId').addEventListener('keypress', e => {
    if (e.key === 'Enter') joinRoom();
});

on('joinBtn', 'click', joinRoom);
