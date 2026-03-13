## Improve workflow - Wish list
Cross IDE, Cross Workstation retention of :
    - Global settings.json 
    - Project level settings.json (if one exists)
    - Global CLAUDE.md file 
    - Project Claude.md file (if one exists)

## Push updates to existing clients 
Client and Server side code must now contain a version. 
- Increase the version number by 0.01 after completed sprints. 
- Add a 0.0.1 if there are any changes to settings.json files or CLAUDE.md files. 
- Start at version 1 for both.

When the MCP client is started, it checks the server. The server will have a note of the lastest client version. If client version is older then warn and automatically update the necessary files.

