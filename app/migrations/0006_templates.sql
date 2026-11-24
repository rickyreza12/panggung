CREATE TABLE software_template_versions (
    name TEXT NOT NULL,
    version TEXT NOT NULL,
    title TEXT NOT NULL,
    description TEXT NOT NULL,
    stack TEXT NOT NULL CHECK (stack IN ('rust', 'dotnet', 'python', 'node', 'react')),
    parameters TEXT NOT NULL CHECK (json_valid(parameters) AND json_type(parameters) = 'array'),
    PRIMARY KEY (name, version)
);

INSERT INTO software_template_versions (name, version, title, description, stack, parameters)
SELECT name, '1.0.0', title, description, stack,
       '[{"name":"project-name","label":"Project name","type":"string","required":true},{"name":"description","label":"Description","type":"string","required":false}]'
FROM (
    SELECT 'rust-service' AS name, 'Rust service' AS title, 'Declare a new Rust backend service.' AS description, 'rust' AS stack
    UNION ALL SELECT 'dotnet-service', '.NET service', 'Declare a new .NET backend service.', 'dotnet'
    UNION ALL SELECT 'python-service', 'Python service', 'Declare a new Python backend service.', 'python'
    UNION ALL SELECT 'node-service', 'Node.js service', 'Declare a new Node.js backend service.', 'node'
    UNION ALL SELECT 'react-app', 'React application', 'Declare a new React frontend application.', 'react'
);
