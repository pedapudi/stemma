-- Mini corpus exercising the mention classes from the README:
-- nickname, abbreviation, description, association.
-- Used by golden tests; loaded into a scratch SQLite DB at test time.

CREATE TABLE offices (
    id   INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    city TEXT NOT NULL
);

CREATE TABLE people (
    id        INTEGER PRIMARY KEY,
    name      TEXT NOT NULL,
    office_id INTEGER REFERENCES offices(id)
);

CREATE TABLE teams (
    id      INTEGER PRIMARY KEY,
    name    TEXT NOT NULL,
    lead_id INTEGER REFERENCES people(id)
);

CREATE TABLE team_members (
    team_id   INTEGER REFERENCES teams(id),
    person_id INTEGER REFERENCES people(id),
    PRIMARY KEY (team_id, person_id)
);

CREATE TABLE reports (
    id        INTEGER PRIMARY KEY,
    title     TEXT NOT NULL,
    office_id INTEGER REFERENCES offices(id),
    quarter   TEXT NOT NULL,   -- e.g. '2025Q3'
    revenue   REAL NOT NULL
);

CREATE TABLE shipments (
    id      INTEGER PRIMARY KEY,
    team_id INTEGER REFERENCES teams(id),
    item    TEXT NOT NULL,
    shipped TEXT NOT NULL      -- ISO date
);

INSERT INTO offices VALUES
    (17, 'Seattle - Northgate', 'Seattle'),
    (18, 'Portland Downtown', 'Portland'),
    (19, 'Crown Building', 'New York');

INSERT INTO people VALUES
    (1, 'Wei Chen', 17),
    (2, 'Dana Chen', 18),
    (3, 'Priya Natarajan', 17),
    (4, 'Sam Okafor', 19);

INSERT INTO teams VALUES
    (42, 'Query Engines', 1),
    (43, 'Billing', 2),
    (44, 'Holdings Research', 4);

INSERT INTO team_members VALUES
    (42, 1), (42, 3),
    (43, 2),
    (44, 4);

INSERT INTO reports VALUES
    (100, 'Q3 revenue summary', 17, '2025Q3', 1250000.0),
    (101, 'Q3 revenue summary', 18, '2025Q3', 890000.0),
    (102, 'Q4 forecast', 17, '2025Q4', 1400000.0);

INSERT INTO shipments VALUES
    (200, 42, 'vector index v2', '2025-09-12'),
    (201, 42, 'query planner rewrite', '2025-10-03'),
    (202, 43, 'invoicing revamp', '2025-08-21');
