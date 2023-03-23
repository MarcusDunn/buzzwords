curl localhost:3000/user/marcus/post/My%201st%20Post/comment/:comment_id/like -H 'Content-Type: application/json' --data '{
  "liker": "marcus"
}'