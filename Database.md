We need to do the following. 

*** CRITICAL: DATALOSS is expected during this change. This is acceptable. ***

*** NOTE: I am not knowledgeable on Qdrant DBs. However I am familiar with relational dbs, note this is the logic I am using. Please convert to Qdrant ***

Global Enums 
id,status[(0,'current'),(1,'done'), (2,'in-progress')]
id,projecttype[(0,'Personal'),(1,'Production'), (2,'Research')]
id,projectphase[(0,'Brainstorming'),(1,'Plan'), (2,'In-progress'), (3, 'Abandoned'), (4, 'Complete')]
id,scope[(0,'Project'),(1,'Global')]

1. Ensure everything stored to the db is date and timestamped.
2. Delete existing tables. No need to back up.
3. Rearrange memories into dedicated tables
    - sprints - fields [sprintID][created/datetime][projectID][Sprint #][Sprint Name][statusID] 
        - [Child Table] increment: [taskID][created/datetime][sprintID][taskDescription][AcceptanceCriteria][statusID]
        - [Child Table] increment_results: [taskID][created/datetime][AcceptanceCriteria][Summary of what was done]
    - topology - fields [topologyID][created/datetime][ServerName/WorkstationName][IP][Ports] 
        - [Child Table] services [serviceID][created/datetime][topologyID][serviceName][projectID][service descripton][url][port]
        - [Child Table] topology_logs [topologylogID][created/datetime][change description]
        - Topology is Global, single Homelab which contains multiple machines/services/projects
    - user_info - [userID][created/datetime][userName][user_memory] -- Memory Table
    - task - [taskID][userID][created/datetime][task summary][serviceID]
g
## These load on session start ensure hooks are generated for these.
    - projects - [projectID][created/datetime][project_name][projectTypeID][projectPhaseID][Description][projectAuthor] -- Reference table
    - sessions - [sessionID][created/datetime][topologyID][projectID][userID][sessionSummary]
    - ide: [ide_id][topologyID][ide_name][yggdrasil_client_version] 
    - ide_configs: [ide_id],[created/datetime][Copy settings.json][scopeid][path] (keep track of all config, settings, agent, workflow and memory files for each IDE)

After changing the DB, trace all endpoints that made use of the old tables and update them to make use of the new database. 

Build and Deploy. Restart VSCode.

**Critcal** Acceptance Criteria: 
Call Yggdrasil each memory tools x10, plus 10x for each table. All must pass. 

Delete test data -- (empty db again)
## NOTES - For projectID, if a service used is not an internally build service. Create a project where the ProjectName, Project Author is the service name (ex. Home Assistant)f

*** Note: I will be referring to primary key of all the memory tables as memoryID.***
*** I note that the QRant organization is fundementally different

*** Solving Novelty Gate threshold issue *** 
Change gate logic: Gate notifies calling AI of potential memory overwrite:
    - Returns memory match + memoryID
    - AI decides if memory is the same as the memory being stored. 
        - If same memory - update 
        - If different memory - create new 
        - If AI can't figure it out confidently, ask user. 
    - When retriving memories, retrive lastest records.

Poke holes in the logic above. Ensure instructions and logic is sound then build plan for implementation.  
