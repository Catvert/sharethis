-- Create rooms table
CREATE TABLE IF NOT EXISTS rooms (
    id TEXT PRIMARY KEY NOT NULL,
    content TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- Create trigger to update updated_at
CREATE TRIGGER IF NOT EXISTS rooms_update_timestamp
    AFTER UPDATE ON rooms
    FOR EACH ROW
BEGIN
    UPDATE rooms
    SET updated_at = CURRENT_TIMESTAMP
    WHERE id = NEW.id;
END;
