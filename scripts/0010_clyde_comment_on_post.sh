curl -v localhost:3000/user/marcus/post/My%201st%20Post/comment/new -H 'Content-Type: application/json' --data '{
  "title": "My 1st Comment",
  "content": "this is a comment",
  "author": "clyde"
}'