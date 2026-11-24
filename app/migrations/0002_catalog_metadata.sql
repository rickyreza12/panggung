ALTER TABLE components ADD COLUMN namespace TEXT NOT NULL DEFAULT 'default' CHECK (namespace = 'default');
ALTER TABLE components ADD COLUMN component_type TEXT;
ALTER TABLE components ADD COLUMN lifecycle TEXT;
ALTER TABLE components ADD COLUMN descriptor TEXT CHECK (descriptor IS NULL OR json_valid(descriptor));
